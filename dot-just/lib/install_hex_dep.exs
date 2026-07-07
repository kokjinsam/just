defmodule Just.InstallHexDep do
  @moduledoc false

  def main([mix_path, package, version]) do
    validate_package!(package)

    source = File.read!(mix_path)
    lines = String.split(source, "\n", trim: false)
    deps = deps(source)

    updated =
      if package in deps.packages do
        update_dependency(lines, deps.range, package, version)
      else
        insert_dependency(lines, deps.range, package, version)
      end

    _ = Code.string_to_quoted!(updated, columns: true, token_metadata: true)

    if package not in deps(updated).packages do
      Mix.raise("Hex dependency #{inspect(package)} was not added to #{mix_path}")
    end

    File.write!(mix_path, updated)
    IO.puts("Added #{package} #{version} to #{mix_path}")
  end

  def main(_args) do
    Mix.raise("usage: install_hex_dep.exs MIX_EXS PACKAGE VERSION")
  end

  defp validate_package!(package) do
    if Regex.match?(~r/^[a-z][a-z0-9_]*$/, package) do
      :ok
    else
      Mix.raise("unsupported Hex package name: #{inspect(package)}")
    end
  end

  defp deps(source) do
    ast = Code.string_to_quoted!(source, columns: true, token_metadata: true)

    {_ast, deps} =
      Macro.prewalk(ast, nil, fn
        {:defp, meta, [{:deps, _call_meta, nil}, [do: body]]} = node, nil ->
          packages =
            body
            |> List.wrap()
            |> Enum.map(&dependency_package/1)
            |> Enum.reject(&is_nil/1)

          range = {meta[:do][:line], meta[:end][:line]}
          {node, %{packages: packages, range: range}}

        node, acc ->
          {node, acc}
      end)

    deps || Mix.raise("could not find defp deps in mix.exs")
  end

  defp dependency_package({:{}, _meta, [name | _rest]}) when is_atom(name), do: Atom.to_string(name)
  defp dependency_package({name, _version}) when is_atom(name), do: Atom.to_string(name)
  defp dependency_package({name, _version, _opts}) when is_atom(name), do: Atom.to_string(name)
  defp dependency_package(_dep), do: nil

  defp update_dependency(lines, range, package, version) do
    {start_line, end_line} = dependency_lines!(lines, range, package)

    original =
      lines
      |> Enum.slice((start_line - 1)..(end_line - 1))
      |> Enum.join("\n")

    atom = atom_literal(package)
    pattern = ~r/(\{\s*#{Regex.escape(atom)}\s*,\s*)"([^"]*)"/

    updated_dependency =
      Regex.replace(
        pattern,
        original,
        fn _match, prefix, _old_version ->
          prefix <> inspect(version)
        end,
        global: false
      )

    if updated_dependency == original do
      Mix.raise("could not update #{package}; expected a versioned dependency tuple")
    end

    replace_lines(lines, start_line, end_line, String.split(updated_dependency, "\n", trim: false))
  end

  defp insert_dependency(lines, range, package, version) do
    close_line = deps_close_line!(lines, range)
    previous_dependency_line = previous_dependency_line!(lines, close_line)
    new_dependency = dependency_line(package, version)

    lines
    |> ensure_trailing_comma(previous_dependency_line)
    |> insert_line(close_line, new_dependency)
    |> Enum.join("\n")
  end

  defp dependency_lines!(lines, {deps_start, deps_end}, package) do
    escaped = Regex.escape(package)
    start_regex = ~r/^\s*\{:(#{escaped})\b|^\s*\{:"#{escaped}"/
    next_entry_regex = ~r/^\s*\{:(\w|")/
    close_list_regex = ~r/^\s*\]/

    start_line =
      deps_start..deps_end
      |> Enum.find(fn line_number -> Regex.match?(start_regex, Enum.at(lines, line_number - 1, "")) end)
      |> Kernel.||(Mix.raise("could not locate #{package} entry lines in mix.exs"))

    end_line =
      Enum.find_value((start_line + 1)..deps_end, deps_end, fn line_number ->
        line = Enum.at(lines, line_number - 1, "")

        cond do
          Regex.match?(next_entry_regex, line) -> line_number - 1
          Regex.match?(close_list_regex, line) -> line_number - 1
          true -> nil
        end
      end)

    {start_line, end_line}
  end

  defp deps_close_line!(lines, {deps_start, deps_end}) do
    deps_start..deps_end
    |> Enum.find(fn line_number -> Regex.match?(~r/^\s*\]/, Enum.at(lines, line_number - 1, "")) end)
    |> Kernel.||(Mix.raise("could not locate deps list closing line"))
  end

  defp previous_dependency_line!(lines, close_line) do
    (close_line - 1)..1//-1
    |> Enum.find(fn line_number -> String.trim(Enum.at(lines, line_number - 1, "")) != "" end)
    |> Kernel.||(Mix.raise("could not locate previous dependency line"))
  end

  defp ensure_trailing_comma(lines, line_number) do
    List.update_at(lines, line_number - 1, fn line ->
      if String.ends_with?(String.trim_trailing(line), ",") do
        line
      else
        line <> ","
      end
    end)
  end

  defp insert_line(lines, line_number, line) do
    List.insert_at(lines, line_number - 1, line)
  end

  defp replace_lines(lines, start_line, end_line, replacement) do
    lines
    |> Enum.with_index(1)
    |> Enum.flat_map(fn {line, line_number} ->
      cond do
        line_number == start_line -> replacement
        line_number > start_line and line_number <= end_line -> []
        true -> [line]
      end
    end)
    |> Enum.join("\n")
  end

  defp dependency_line(package, version) do
    "      {#{atom_literal(package)}, #{inspect(version)}}"
  end

  defp atom_literal(package), do: ":#{package}"
end

Just.InstallHexDep.main(System.argv())
