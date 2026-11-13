# Desktop editor

The interface is compiled into Prismestra; it does not start a local HTTP server or load a website. WebView2 renders the window, while the existing Rust session owns scenes, device transports and sensor subscriptions. Source: [embedded host](../src/ui/windows/web_shell.rs), [command bridge](../src/ui/windows/modern.rs), [native form adapter](../src/ui/windows/native_forms.rs).

## Scenes and profiles

A scene in the library contains RGB effect parameters. Editing and previewing it do not change the running scene. Apply explicitly updates the Rust scene. The preview and library thumbnails use the same native Renderer as the engine; their regular sample grid is not a physical device layout. Source: [scene renderer](../src/runtime/scene.rs), [catalog](../src/ui/catalog.rs).

Device topology, output groups, LCD media, screen capture and temperature routes belong to the profile. Open, Save and Export use the existing profile and media handling. Local hardware bindings and credentials remain separate from portable profiles. Applying a connection is an explicit hardware operation; saving its form does not activate it. Source: [profiles](../src/app/document.rs), [connections](../src/ui/windows/connections.rs).

Closing the editor first requests the latest RGB draft and waits for its atomic save. An error or timeout cancels closing instead of discarding changes. Unsaved device/connection/source forms still require Apply or Revert. The resident session's Stop and Exit paths remain distinct. Source: [close handshake](../src/ui/windows/modern.rs), [resident session](../src/ui/windows/resident.rs).

## Runtime and recovery

The new editor requires Windows x64 and Microsoft Edge WebView2 Runtime 105 or newer. Microsoft describes runtime installation and servicing in its [distribution guide](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution). All editor resources are local. Navigation, resource requests, downloads, new windows and permissions are restricted by the host; the browser frontend cannot issue network requests through its page policy. Device backends retain their own explicit network connections.

Use `Prismestra.exe --classic-editor` to open the former Win32 surface. If the new host fails to initialize, its error is shown on that surface. A normal launch requires access to the user's taskbar/tray; the restricted test sandbox does not provide it.

Settings directories now also contain `scene-library.json`, `editor-scene-draft.json` and a `webview-editor` browser data directory. The first two are validated application data, not browser project storage. Invalid existing catalog/draft files are preserved; initialization reports their error. Do not publish a personal settings directory.

## Verification boundaries

For a configured local profile, `Prismestra.exe --project profile.json --state-dir state --start-connected` opens the saved hardware outputs and starts the exact saved temperature routes at startup. The connection file is `state/lighting-local.json`; assets and sensor routes remain in the same state directory as the editor. Missing or incompatible bindings produce an error rather than selecting another device or inventing temperatures. No discovery runs automatically. The flag requires an explicit profile and is forbidden in snapshots and selftests. Without the flag, startup remains a software preview. Source: [startup options](../src/ui/windows.rs), [saved source subscriptions](../src/ui/windows/sources/bindings.rs).

The source has Rust, frontend-IPC and packaging checks. New UI and hardware acceptance must identify the exact tested executable revision. Existing Preview3 device records do not prove a new WebView2 build's physical behavior, interaction, clean-machine readiness or resource use. Source review of controller teardown is not a measurement that browser processes have exited.
