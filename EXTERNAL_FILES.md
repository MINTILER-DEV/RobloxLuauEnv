# External File Loading in RLE

## Overview

RLE now supports loading external files (non-script files) as `StringValue` instances. This allows you to include configuration files, data files, and other resources in your RLE projects.

## Usage

### Directory Structure

Place any files you want to load as `StringValue` instances in an `ExternalData` directory at the root of your project:

```
your_project/
├── ServerScriptService/
│   └── main.server.luau
├── ExternalData/
│   ├── config.json
│   ├── readme.txt
│   └── data/
│       └── seed_data.csv
```

### What Gets Loaded

All files in the `ExternalData` directory become `StringValue` instances named after their filename. The file contents are stored in the `Value` property of each `StringValue`.

**Files are NOT loaded as scripts**, even if they have Lua extensions. So files like `.lua` or `.luau` in the `ExternalData` directory will be treated as data files, not executed code.

### Accessing External Files

You can access external files just like any other instance:

```lua
-- Get from the DataModel
local configFile = game:FindFirstChild("config.json")
if configFile then
	print("Config:", configFile.Value)
end

-- Or from the root (ExternalData contents are at root level)
local readmeFile = game:GetChildren()[1]:FindFirstChild("readme.txt")
if readmeFile then
	print("Readme:", readmeFile.Value)
end
```

### Nested Files in Subdirectories

Files in subdirectories within `ExternalData` are placed in corresponding `Folder` instances:

```
ExternalData/
└── data/
    └── seed_data.csv
```

In Lua:
```lua
local dataFolder = game:FindFirstChild("data")
local seedData = dataFolder:FindFirstChild("seed_data.csv")
print(seedData.Value)
```

### File Content

- Files are loaded as **UTF-8 text**
- File names with any extension are supported
- File content is stored in the `Value` property of the `StringValue`
- Only valid UTF-8 content is supported (binary files will fail)

## Example

See [external_data_example](../external_data_example/) for a complete example.

### Running the Example

```bash
cargo run -- run-server examples/external_data_example
cargo run -- emulate-client examples/external_data_example
```

## Implementation Details

- External files are NOT executed
- They do NOT appear in `ServerStorage` or other services
- They appear at the root level (under `game`)
- Files are sorted alphabetically like regular instances
- External files can be included in packed `.rleimg` images
