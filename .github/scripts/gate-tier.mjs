// Decide which gate tier a CI run owes and the explicit base it keys off — the
// placement AGENTS.md "Commits, releases, and merging" records. Releases are
// batched behind release-plz's release PR, so that PR gets the full sweep (`all`)
// and every other pull request, and every push to main, gets the affected tier.
//
// Reads GITHUB_EVENT_NAME and the payload at GITHUB_EVENT_PATH and writes
// `tier=<affected|all>` and `base=<sha>` (empty for `all`) to GITHUB_OUTPUT,
// saying which on stderr; outside Actions (no GITHUB_OUTPUT) the two lines go to
// stdout instead.
//
// Usage: node .github/scripts/gate-tier.mjs
// Exit status: 0 with a decision; 1 when no decision can be made (the message
// names the cause and the next action).
import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

/** A routing failure carrying the concrete next action for the CI log. */
export class RoutingError extends Error {
  constructor(message, fix) {
    super(message);
    this.fix = fix;
  }
}

/**
 * The branch prefix release-plz opens its release PR from: `pr_branch_prefix` in
 * release-plz.toml when set, else release-plz's default `release-plz-`.
 */
export function releaseBranchPrefix(root = repoRoot) {
  let toml;
  try {
    toml = readFileSync(join(root, "release-plz.toml"), "utf8");
  } catch (err) {
    throw new RoutingError(`cannot read release-plz.toml: ${err.message}`, "check out the full repository.");
  }
  const set = toml.match(/^\s*pr_branch_prefix\s*=\s*"([^"]+)"/m);
  return set ? set[1] : "release-plz-";
}

function git(args, cwd) {
  try {
    return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  } catch (err) {
    throw new RoutingError(
      `git ${args.join(" ")} failed: ${String(err.stderr || err.message).trim()}`,
      "check out with fetch-depth: 0 so the base branch and its history are present.",
    );
  }
}

function resolves(rev, cwd) {
  try {
    git(["rev-parse", "--verify", "--quiet", `${rev}^{commit}`], cwd);
    return true;
  } catch {
    return false;
  }
}

const SAFE_REF = /^[A-Za-z0-9._/-]+$/;
const SHA = /^[0-9a-f]{40}$/;

/** The routing decision for one event, from its payload and the git history in `cwd`. */
export function decide(eventName, payload, { cwd = process.cwd(), root = repoRoot } = {}) {
  if (eventName === "pull_request") {
    const pr = payload.pull_request ?? {};
    const sameRepo = typeof pr.head?.repo?.full_name === "string" && pr.head.repo.full_name === pr.base?.repo?.full_name;
    if (sameRepo && typeof pr.head?.ref === "string" && pr.head.ref.startsWith(releaseBranchPrefix(root))) {
      return { tier: "all", base: "", why: `release PR (${pr.head.ref}): the full sweep, at release-prep` };
    }
    const baseRef = pr.base?.ref;
    if (typeof baseRef !== "string" || !SAFE_REF.test(baseRef) || baseRef.includes("..")) {
      throw new RoutingError(
        `pull_request payload has no usable base ref (${JSON.stringify(baseRef)})`,
        "target a branch whose name is letters, digits and . _ / - only; nothing was run.",
      );
    }
    const base = git(["merge-base", `origin/${baseRef}`, "HEAD"], cwd);
    return { tier: "affected", base, why: `pull request: merge base with origin/${baseRef}` };
  }
  if (eventName === "push") {
    const before = payload.before;
    if (typeof before === "string" && SHA.test(before) && !/^0+$/.test(before) && resolves(before, cwd)) {
      return { tier: "affected", base: before, why: "push: the previous tip (event.before)" };
    }
    return { tier: "affected", base: git(["rev-parse", "HEAD~1"], cwd), why: "push: HEAD~1 (event.before unavailable)" };
  }
  return { tier: "all", base: "", why: `${eventName || "unknown event"}: the full sweep` };
}

function readPayload(path) {
  if (!path) return {};
  try {
    const payload = JSON.parse(readFileSync(path, "utf8"));
    if (payload === null || typeof payload !== "object" || Array.isArray(payload)) throw new Error("not a JSON object");
    return payload;
  } catch (err) {
    throw new RoutingError(
      `the event payload at GITHUB_EVENT_PATH (${path}) is unreadable: ${err.message}`,
      "run this inside a GitHub Actions job (it provides the payload), or point GITHUB_EVENT_PATH at a JSON event.",
    );
  }
}

if (import.meta.main) {
  let decision;
  try {
    decision = decide(process.env.GITHUB_EVENT_NAME ?? "", readPayload(process.env.GITHUB_EVENT_PATH));
  } catch (err) {
    console.error(`gate-tier: ${err.message}`);
    if (err.fix) console.error(`gate-tier: next: ${err.fix}`);
    process.exit(1);
  }
  const lines = `tier=${decision.tier}\nbase=${decision.base}\n`;
  if (!process.env.GITHUB_OUTPUT) {
    process.stdout.write(lines);
  } else {
    try {
      appendFileSync(process.env.GITHUB_OUTPUT, lines);
    } catch (err) {
      console.error(`gate-tier: could not append to GITHUB_OUTPUT (${process.env.GITHUB_OUTPUT}): ${err.message}`);
      process.exit(1);
    }
  }
  console.error(`gate-tier: ${decision.tier} — ${decision.why}`);
}
