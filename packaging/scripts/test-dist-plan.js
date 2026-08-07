const fs = require("node:fs");

const plan = JSON.parse(fs.readFileSync(0, "utf8"));
const text = JSON.stringify(plan);
for (const asset of fs.readFileSync("packaging/fixtures/cargo-dist-assets.txt", "utf8").trim().split("\n")) {
  if (!text.includes(asset)) throw new Error(`cargo-dist plan did not contain expected asset ${asset}`);
}
