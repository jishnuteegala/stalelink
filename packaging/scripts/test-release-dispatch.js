const fs = require("node:fs");
const yaml = require("yaml");

const workflow = yaml.parse(fs.readFileSync(".github/workflows/release.yml", "utf8"));
const dispatch = workflow.on?.workflow_dispatch;
if (!dispatch?.inputs?.tag?.required || dispatch.inputs.tag.type !== "string") {
  throw new Error("release.yml must accept a required string tag through workflow_dispatch");
}
const plan = workflow.jobs?.plan;
for (const key of ["tag", "tag-flag", "publishing"]) {
  if (!String(plan?.outputs?.[key] || "").includes("inputs.tag")) {
    throw new Error(`release.yml plan output ${key} does not use the dispatched tag`);
  }
}
