defmodule Just.RemoveHexDep do
  @moduledoc false

  def main([mix_path, package]) do
    source = File.read!(mix_path)
    lines = String.split(source, "\n", trim: false)
    deps = deps(source)

    if package not in deps.packages do
      Mix.raise("Hex dependency #{inspect(package)} was not found in #{mix_path}")
    end

    {start_line, end_line} = dependency_lines!(lines, deps.range, package)

    updated =
      lines
      |> Enum.with_index(1)
      |> Enum.reject(fn {_line, line_number} -> line_number in start_line..end_line end)
      |> remove_trailing_comma_before_deps_close(deps.range)
      |> Enum.map_join("\n", fn {line, _line_number} -> line end)

    _ = Code.string_to_quoted!(updated, columns: true, token_metadata: true)

    if package in deps(updated).packages do
      Mix.raise("Hex dependency #{inspect(package)} is still present in #{mix_path}")
    end

    File.write!(mix_path, updated)
    IO.puts("Removed #{package} from #{mix_path}")
  end

  def main(_args) do
    Mix.raise("usage: remove_hex_dep.exs MIX_EXS PACKAGE")
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

  defp remove_trailing_comma_before_deps_close(indexed_lines, {deps_start, deps_end}) do
    close_index =
      Enum.find_index(indexed_lines, fn {line, line_number} ->
        line_number >= deps_start and line_number <= deps_end and Regex.match?(~r/^\s*\]/, line)
      end)

    if is_nil(close_index) do
      Mix.raise("could not locate deps list closing line")
    end

    previous_index =
      Enum.find((close_index - 1)..0//-1, fn index ->
        {line, _line_number} = Enum.at(indexed_lines, index)
        String.trim(line) != ""
      end)

    if is_nil(previous_index) do
      Mix.raise("could not locate previous dependency line")
    end

    List.update_at(indexed_lines, previous_index, fn {line, line_number} ->
      {String.replace(line, ~r/,\s*$/, ""), line_number}
    end)
  end
end

Just.RemoveHexDep.main(System.argv())
