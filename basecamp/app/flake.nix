{
  description = "The Agent app for logos-mcp in Basecamp: connect, permissions and activity";
  # Module-backed views ("backend": "module") are on builder master, not yet in a release.
  inputs.logos-module-builder.url = "github:logos-co/logos-module-builder/5f74cdb3cecacde054c68d72fcc0d2cfe2195900";
  # The MCP module this app controls, from this repo.
  inputs.logos_mcp.url = "path:..";
  outputs = inputs@{ logos-module-builder, ... }:
    logos-module-builder.lib.mkLogosQmlModule {
      src = ./.;
      configFile = ./metadata.json;
      flakeInputs = inputs;
    };
}
