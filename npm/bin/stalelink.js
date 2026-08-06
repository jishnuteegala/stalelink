#!/usr/bin/env node
"use strict";

const { spawnSync } = require("node:child_process");
const { packageName } = require("../lib/platform");

const binary = process.platform === "win32" ? "stalelink.exe" : "stalelink";
const target = require.resolve(`${packageName(process.platform, process.arch)}/bin/${binary}`);
const result = spawnSync(target, process.argv.slice(2), { stdio: "inherit" });
if (result.error) throw result.error;
process.exit(result.status ?? 1);
