fn main() {
    tauri_build::build();

    // Embed the requireAdministrator manifest so the .exe asks for
    // UAC on launch. Without this the bootstrapper inherits the
    // user's standard token even when NSIS installed it elevated,
    // and install-silent.ps1 fails on its #Requires line.
    #[cfg(windows)]
    embed_resource::compile("app.rc", embed_resource::NONE);
}
