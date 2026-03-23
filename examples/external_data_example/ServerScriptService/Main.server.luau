--!strict
-- Example script showing how to access external data files

-- External files become StringValue instances
local config = game:FindFirstChild("config.json")
local readme = game:FindFirstChild("readme.txt")

if config then
	print("Config file content:")
	print(config.Value)
end

if readme then
	print("Readme file content:")
	print(readme.Value)
end
