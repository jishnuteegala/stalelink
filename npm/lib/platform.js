"use strict";

const packages = {
  "darwin-arm64": "@stalelink/darwin-arm64",
  "darwin-x64": "@stalelink/darwin-x64",
  "linux-arm64": "@stalelink/linux-arm64",
  "linux-x64": "@stalelink/linux-x64",
  "win32-arm64": "@stalelink/win32-arm64",
  "win32-x64": "@stalelink/win32-x64",
};

function packageName(platform, arch) {
  const name = packages[`${platform}-${arch}`];
  if (!name) throw new Error(`stalelink does not support ${platform}-${arch}`);
  return name;
}

module.exports = { packageName };
