// Enforce the project graph's module boundaries (tools/project-boundaries.json).
//
// Nx's own enforce-module-boundaries rule is an ESLint rule over JS imports; the
// edges that matter here are Cargo dependencies and Nx implicitDependencies, so
// this reads both layers directly:
//   * every Cargo workspace member (`cargo metadata --no-deps`) must have a
//     project.json beside its Cargo.toml, and each of its path dependencies on
//     another member (normal, dev or build) is an edge;
//   * every project.json's implicitDependencies are edges too.
// Each project must carry exactly one `type:*` tag, and every edge must be
// allowed by the constraint for the source project's tag. When Nx is installed
// in the tree being checked, the implicit edges read here must also match the
// graph `nx graph` resolves, so the two readings cannot drift.
//
// Usage: node tools/check-project-boundaries.mjs [--root <workspace dir>]
// Exit status: 0 (quiet) when every edge is allowed; 1 with each violation
// printed; 2 on a usage error.
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/** `[--root <existing dir>]`, nothing else; the default is this repository. */
function parseRoot(argv, fallback) {
  if (argv.length === 0) return fallback;
  if (argv.length === 2 && argv[0] === "--root" && existsSync(argv[1]) && statSync(argv[1]).isDirectory()) {
    return resolve(argv[1]);
  }
  console.error(`usage: node tools/check-project-boundaries.mjs [--root <existing directory>] (got: ${argv.join(" ") || "nothing"})`);
  process.exit(2);
}

const root = parseRoot(process.argv.slice(2), resolve(here, ".."));

function fail(lines) {
  for (const line of lines) console.error(`project-boundaries: ${line}`);
  process.exit(1);
}

// Project dirs relative to the root with `/` on every platform; Cargo reports
// canonical paths (macOS /private/var, Windows 8.3 names), so compare canonically.
const canon = (p) => {
  try {
    return realpathSync.native(p);
  } catch {
    return resolve(p);
  }
};
const relDir = (p) => relative(canon(root), canon(p)).split("\\").join("/") || ".";

function readJson(file) {
  try {
    return JSON.parse(readFileSync(join(root, file), "utf8"));
  } catch (err) {
    fail([`${file} is not readable JSON: ${err.message}`]);
  }
}

const isStrings = (v) => Array.isArray(v) && v.every((x) => typeof x === "string" && x.length > 0);

// The policy: a non-empty list of { sourceTag, onlyDependOnTags }, one per tag.
const rules = readJson("tools/project-boundaries.json");
if (rules === null || typeof rules !== "object" || !Array.isArray(rules.depConstraints) || rules.depConstraints.length === 0) {
  fail(["tools/project-boundaries.json must hold a non-empty depConstraints array."]);
}
const constraints = new Map();
for (const c of rules.depConstraints) {
  if (typeof c?.sourceTag !== "string" || !c.sourceTag.startsWith("type:") || !isStrings(c.onlyDependOnTags)) {
    fail([`malformed constraint in tools/project-boundaries.json: ${JSON.stringify(c)}`]);
  }
  if (constraints.has(c.sourceTag)) fail([`tools/project-boundaries.json constrains ${c.sourceTag} twice.`]);
  constraints.set(c.sourceTag, c.onlyDependOnTags);
}

let metadata;
try {
  metadata = JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps", "--offline"], {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    }),
  );
} catch (err) {
  fail([`'cargo metadata' failed: ${err.stderr || err.message}`, "fix the manifests so it resolves, then re-run."]);
}

const errors = [];

const projectFiles = execFileSync(
  "git",
  ["ls-files", "--cached", "--others", "--exclude-standard", "--", "project.json", "**/project.json"],
  { cwd: root, encoding: "utf8" },
)
  .split("\n")
  .filter((f) => f && !f.startsWith("node_modules/"));

const projects = new Map(); // name -> { dir, tags, implicit }
const byDir = new Map(); // dir -> name
for (const file of projectFiles) {
  const json = readJson(file);
  if (json === null || typeof json !== "object" || Array.isArray(json)) fail([`${file} must hold a JSON object.`]);
  const tags = json.tags ?? [];
  const implicit = json.implicitDependencies ?? [];
  if (typeof json.name !== "string" || !/^[a-z0-9][a-z0-9-]*$/.test(json.name)) {
    fail([`${file} must name its project (lowercase letters, digits and -), got ${JSON.stringify(json.name)}.`]);
  }
  if (!isStrings(tags) || !isStrings(implicit)) {
    fail([`${file}: tags and implicitDependencies must be arrays of strings.`]);
  }
  const dir = dirname(file);
  if (projects.has(json.name)) fail([`project name ${json.name} is declared twice (${projects.get(json.name).dir} and ${dir}).`]);
  projects.set(json.name, { dir, tags, implicit });
  byDir.set(dir, json.name);
}

// implicitDependencies the way Nx reads them: names, `*` globs, `tag:<tag>`, and `!` exclusions.
function expandImplicit(name, patterns) {
  const glob = (p) => new RegExp(`^${p.split("*").map((x) => x.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*")}$`);
  const matching = (p) =>
    p.startsWith("tag:")
      ? [...projects].filter(([, project]) => project.tags.includes(p.slice(4))).map(([n]) => n)
      : [...projects.keys()].filter((n) => glob(p).test(n));
  const selected = new Set();
  for (const p of patterns.filter((x) => !x.startsWith("!"))) {
    const matches = matching(p);
    if (matches.length === 0) errors.push(`${name} names unknown implicit dependency ${p}.`);
    for (const m of matches) if (m !== name) selected.add(m);
  }
  for (const p of patterns.filter((x) => x.startsWith("!"))) {
    for (const n of matching(p.slice(1))) selected.delete(n);
  }
  return [...selected];
}

const typeTag = (name) => {
  const tags = projects.get(name).tags.filter((t) => t.startsWith("type:"));
  if (tags.length !== 1) {
    errors.push(`${name} must carry exactly one type:* tag (has ${JSON.stringify(tags)}).`);
    return undefined;
  }
  if (!constraints.has(tags[0])) {
    errors.push(`${name}'s tag ${tags[0]} has no constraint in tools/project-boundaries.json.`);
    return undefined;
  }
  return tags[0];
};

const isPkg = (p) =>
  p !== null &&
  typeof p === "object" &&
  typeof p.id === "string" &&
  typeof p.name === "string" &&
  typeof p.manifest_path === "string" &&
  Array.isArray(p.dependencies) &&
  p.dependencies.every((d) => d !== null && typeof d === "object" && typeof d.name === "string" && (d.path === undefined || typeof d.path === "string"));
if (!Array.isArray(metadata?.packages) || !metadata.packages.every(isPkg) || !isStrings(metadata.workspace_members)) {
  fail(["'cargo metadata' returned an unexpected shape; check the cargo version, then re-run."]);
}

const edges = []; // [from, to, how]
const memberDirs = new Map(); // manifest dir -> project name
for (const pkg of metadata.packages) {
  const dir = relDir(dirname(pkg.manifest_path));
  const name = byDir.get(dir);
  if (!name) {
    errors.push(`Cargo member ${pkg.name} (${dir}/Cargo.toml) has no project.json beside it.`);
    continue;
  }
  memberDirs.set(dir, name);
}
for (const pkg of metadata.packages) {
  const from = memberDirs.get(relDir(dirname(pkg.manifest_path)));
  if (!from) continue;
  for (const dep of pkg.dependencies) {
    if (!dep.path) continue;
    const to = memberDirs.get(relDir(dep.path));
    if (to && to !== from) edges.push([from, to, `Cargo ${dep.kind ?? "normal"} dependency on ${dep.name}`]);
  }
}
const implicitEdges = new Set();
for (const [name, project] of projects) {
  for (const dep of expandImplicit(name, project.implicit)) {
    edges.push([name, dep, "implicitDependencies"]);
    implicitEdges.add(`${name} -> ${dep}`);
  }
}

// Drift gate: this reading of implicitDependencies must match the graph Nx
// itself resolves, whenever Nx is installed in the tree being checked.
if (existsSync(join(root, "node_modules/.bin/nx"))) {
  const scratch = mkdtempSync(join(tmpdir(), "nx-graph-"));
  const graphFile = join(scratch, "graph.json");
  try {
    execFileSync("bash", ["scripts/nx", "graph", `--file=${graphFile}`], { cwd: root, stdio: ["ignore", "ignore", "pipe"] });
  } catch (err) {
    fail([`'nx graph' failed: ${err.stderr || err.message}`, "run 'just bootstrap', then re-run."]);
  }
  const deps = JSON.parse(readFileSync(graphFile, "utf8"))?.graph?.dependencies;
  rmSync(scratch, { recursive: true, force: true });
  const wellFormed =
    deps !== null &&
    typeof deps === "object" &&
    Object.values(deps).every((list) => Array.isArray(list) && list.every((d) => typeof d?.target === "string" && typeof d?.type === "string"));
  if (!wellFormed) fail(["'nx graph' produced a graph of an unexpected shape; check the Nx version in package.json, then re-run."]);
  const nxEdges = new Set(
    Object.entries(deps).flatMap(([from, list]) => list.filter((d) => d.type === "implicit").map((d) => `${from} -> ${d.target}`)),
  );
  const only = (a, b) => [...a].filter((e) => !b.has(e));
  for (const e of only(nxEdges, implicitEdges)) errors.push(`Nx resolves ${e}, which this checker did not; teach expandImplicit that pattern.`);
  for (const e of only(implicitEdges, nxEdges)) errors.push(`this checker resolves ${e}, which Nx does not; align expandImplicit with Nx.`);
}

for (const name of projects.keys()) typeTag(name);
for (const [from, to, how] of edges) {
  const fromTag = typeTag(from);
  const toTag = typeTag(to);
  if (!fromTag || !toTag) continue;
  const allowed = constraints.get(fromTag);
  if (!allowed.includes(toTag)) {
    errors.push(`${from} (${fromTag}) may not depend on ${to} (${toTag}) — found ${how}. ${fromTag} may depend only on ${allowed.join(", ")}.`);
  }
}

if (errors.length) {
  fail([...new Set(errors), "see tools/project-boundaries.json for the allowed edges."]);
}
