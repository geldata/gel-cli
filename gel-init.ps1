#!/usr/bin/env pwsh

# A simple Powershell script to download and install the EdgeDB CLI.
#
# Copyright 2021-present EdgeDB Inc. and the EdgeDB authors.
# Licensed under the Apache License, Version 2.0 (the "License");

&{

# Bail immediately on any error.
$ErrorActionPreference = 'Stop'
# Prevent the pointless progress overlay produced by iwr.
$ProgressPreference = 'SilentlyContinue'
# Ensure proper transport security.
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$BaseUrl = "https://packages.edgedb.com/dist"
$DistArch = "x86_64"
If ($Channel -eq $null) {
  $DistSuf = ""
} Else {
  $DistSuf = ".$Channel"
}
$DownloadUrl = "$BaseUrl/${DistArch}-pc-windows-msvc${DistSuf}/edgedb-cli.exe"

Write-Output "Downloading installer..."

$TempRoot = [System.IO.Path]::GetTempPath()
$TempStem = [System.IO.Path]::GetRandomFileName()
$TempDir = Join-Path $TempRoot $TempStem
$CliExe = Join-Path $TempDir "edgedb-init.exe"
New-Item $TempDir -ItemType Directory | Out-Null

Invoke-WebRequest $DownloadUrl -OutFile $CliExe -UseBasicParsing

Start-Process $CliExe -ArgumentList "--no-wait-for-exit-prompt" -NoNewWindow -Wait

# Make PATH modifications actual in current session.
$User = [EnvironmentVariableTarget]::User
$Path = [Environment]::GetEnvironmentVariable('Path', $User)
$Env:Path += ";$Path"

}
