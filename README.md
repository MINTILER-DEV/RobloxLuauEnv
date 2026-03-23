# RobloxLuauEnv

`RobloxLuauEnv` is a Rust + Luau CLI that emulates a Roblox-like object model, script runtime, and service layout without physics simulation.

## CLI

```powershell
cargo run -- run-server examples\demo_project
cargo run -- emulate-client examples\demo_project
cargo run -- gui
cargo run -- pack examples\demo_project examples\demo_project.rleimg
cargo run -- unpack examples\demo_project.rleimg examples\unpacked_demo
cargo run -- export-rbxlx examples\demo_project examples\demo_project.rbxlx
```

You can run either a project directory or a `.rleimg` image with `run-server` and `emulate-client`.
Those two commands now stay alive like a host process and exit on `Ctrl+C`.

## Desktop GUI

`cargo run -- gui` opens the desktop editor shell for RLE.

- explorer on the left
- code editor with tabs in the main area
- console in the lower-right panel
- `ScreenGui` tab reserved for future rendering work
- open folders or `.rleimg` images without unpacking images into the current directory
- topbar actions for server/client run, save, image export, RBXLX export, `Add Instance`, and opening the active script in VS Code
- light and dark themes with rounded UI panels

## Project layout

Files are loaded into an instance tree that mirrors the folder structure.

- root folders matching service names like `Workspace`, `ReplicatedStorage`, `ServerStorage`, `Players`, or `Lighting` are loaded under those services
- other folders become `Folder` instances under `game`
- if a non-service folder contains `init.luau` or `init.lua`, that folder becomes a `ModuleScript` named after the folder, and the rest of that folder's contents become children of that module
- `*.luau` and `*.lua` become `ModuleScript`
- `*.server.luau` and `*.server.lua` become `Script`
- `*.client.luau` and `*.client.lua` become `LocalScript`

Server mode auto-runs `Script` instances. Client mode auto-runs `LocalScript` instances. `ModuleScript` instances can be loaded with `require(moduleScriptInstance)`.

## External files (ExternalData)

Files placed in an `ExternalData` directory at the root of your project are loaded as `StringValue` instances. The filename becomes the instance name, and the file content is stored in the `Value` property. This allows you to include configuration files, data files, and other resources in your projects.

```
project/
├── ExternalData/
│   ├── config.json
│   ├── data.txt
│   └── subfolder/
│       └── seed.csv
```

See [EXTERNAL_FILES.md](EXTERNAL_FILES.md) for more details and examples.

## `.rleimg`

`.rleimg` stands for `RobloxLuaEnvironment` image. It is a portable packaged snapshot of a project directory that the CLI can pack, unpack, and run directly.

## RBXLX export

`export-rbxlx` writes the current project layout to a Roblox XML place file. It exports the static instance tree defined by the project structure, including script sources and `init.luau` folder-to-module behavior.

## Current runtime coverage

- `game`, `workspace`, and built-in services including `Workspace`, `ReplicatedStorage`, `ServerStorage`, `Lighting`, `Players`, `RunService`, `HttpService`, and `TweenService`
- instances, parenting, descendants, cloning, destroying, `FindFirstChild`, `GetChildren`, `GetDescendants`, `GetFullName`, and property changed signals
- signals/events through `:Connect(...)`, `:Once(...)`, and connection `:Disconnect()`
- `Vector3.new(...)` and `Color3.new(...)`
- `task.wait`, `task.spawn`, `task.defer`, and `task.delay`
- `HttpService:GetAsync`, `HttpService:PostAsync`, `HttpService:JSONEncode`, and `HttpService:JSONDecode`
- `RunService:IsClient()` and `RunService:IsServer()`
- `Players:GetPlayers()` and `Players.LocalPlayer` in client emulation mode

## Client emulation

`emulate-client` does not create a character.

- `Players.CharacterAutoLoads` is forced to `false`
- `Players.LocalPlayer` exists
- `Player:LoadCharacter()` errors by design
- `ServerStorage` and `ServerScriptService` are not mounted into the client view
- client edits to server-replicated `Part` instances are treated as local-only unless the server assigned network ownership to `LocalPlayer`

## Network ownership

- `Part:SetNetworkOwner(playerOrNil)` is server-only
- `Part:GetNetworkOwner()` returns the owning `Player` when one is assigned
- client-created parts are treated as client-authoritative inside client emulation

## Physics policy

- `Part.Anchored` always remains `true`
- setting `Part.Anchored = false` logs a warning and keeps the stored value `true`
- setting `Part.CanCollide = true` is allowed but logs a warning because collision simulation is not implemented
- placeholder touch events exist for compatibility, but they are not fired automatically

## Example

An example project lives in [examples/demo_project](examples/demo_project).
