--!strict
-- Example script showing how to access external data files

local ReplicatedStorage = game:GetService("ReplicatedStorage")

local function findExternal(name: string): StringValue?
    local value = game:FindFirstChild(name)
    if value and value:IsA("StringValue") then
        return value
    end
    return nil
end

local config = findExternal("config.json")
local readme = findExternal("readme.txt")
local testElf = findExternal("test.elf")
local helloElf = findExternal("hello_syscall.elf")

if config then
	print("Config file content:")
	print(config.Value)
end

if readme then
	print("Readme file content:")
	print(readme.Value)
end

if testElf then
	print(string.format("Loaded test.elf (%d bytes)", #testElf.Value))
end

if helloElf then
	print(string.format("Loaded hello_syscall.elf (%d bytes)", #helloElf.Value))
end

local elfLuauModule = ReplicatedStorage:FindFirstChild("ElfLuau")
if helloElf and elfLuauModule and elfLuauModule:IsA("ModuleScript") then
	local ElfLuau = require(elfLuauModule)
	local result = ElfLuau.runExternal(helloElf, { argv = { "/hello_syscall.elf" } })
	print("hello_syscall.elf stdout:")
	print(result.stdout)
	print("hello_syscall.elf exit code:", result.exitCode)
elseif helloElf then
	print("ReplicatedStorage.ElfLuau is not present, so the ELF is only being loaded as external data.")
end
