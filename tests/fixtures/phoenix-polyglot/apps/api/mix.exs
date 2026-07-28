defmodule Fixture.MixProject do
  use Mix.Project

  def project,
    do: [
      app: :backend,
      version: "0.1.0",
      test_coverage: [tool: ExCoveralls]
    ]

  defp deps, do: [{:excoveralls, "~> 0.18", only: :test}]
end
