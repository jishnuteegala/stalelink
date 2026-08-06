"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const { packageName } = require("../lib/platform");

test("resolves release packages for every supported operating system", () => {
  assert.equal(packageName("darwin", "arm64"), "@stalelink/darwin-arm64");
  assert.equal(packageName("darwin", "x64"), "@stalelink/darwin-x64");
  assert.equal(packageName("linux", "arm64"), "@stalelink/linux-arm64");
  assert.equal(packageName("linux", "x64"), "@stalelink/linux-x64");
  assert.equal(packageName("win32", "arm64"), "@stalelink/win32-arm64");
  assert.equal(packageName("win32", "x64"), "@stalelink/win32-x64");
});

test("rejects unsupported platforms", () => {
  assert.throws(() => packageName("freebsd", "x64"), /does not support/);
});
