print("client booting")

local players = game:GetService("Players")
print("CharacterAutoLoads:", players.CharacterAutoLoads)
print("LocalPlayer:", players.LocalPlayer and players.LocalPlayer.Name or "nil")

local runService = game:GetService("RunService")
print("IsClient:", runService:IsClient(), "IsServer:", runService:IsServer())

local ok, err = pcall(function()
    players.LocalPlayer:LoadCharacter()
end)

print("LoadCharacter ok:", ok)
if not ok then
    warn(err)
end
