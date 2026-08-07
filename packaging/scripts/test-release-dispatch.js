const fs = require("node:fs");

const text = fs.readFileSync(".github/workflows/release.yml", "utf8");

const dispatchBlock = text.match(
  /workflow_dispatch:\n(?:[ \t]+.*\n)+?[ \t]+tag:\n((?:[ \t]{8,}.*\n)+)/,
);
if (!dispatchBlock) {
  throw new Error("release.yml must declare a workflow_dispatch tag input");
}
if (
  !/required:\s*true/.test(dispatchBlock[1]) ||
  !/type:\s*string/.test(dispatchBlock[1])
) {
  throw new Error(
    "release.yml must accept a required string tag through workflow_dispatch",
  );
}

const planOutputs = text.match(
  /\n {2}plan:\n(?:.*\n)*? {4}outputs:\n((?: {6}.*\n)+)/,
);
if (!planOutputs) {
  throw new Error("release.yml plan job must declare outputs");
}
for (const key of ["tag", "tag-flag", "publishing"]) {
  const line = planOutputs[1]
    .split("\n")
    .find((l) => l.trim().startsWith(`${key}:`));
  if (!line || !line.includes("inputs.tag")) {
    throw new Error(
      `release.yml plan output ${key} does not use the dispatched tag`,
    );
  }
}
