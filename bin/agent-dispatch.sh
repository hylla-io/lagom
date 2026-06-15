#!/usr/bin/env bash
#
# bin/agent-dispatch.sh — chain-mode subagent dispatcher
#
# Usage:
#   echo "<task prompt>" | ./bin/agent-dispatch.sh --role ta-go-builder
#   ./bin/agent-dispatch.sh --role ta-go-builder --prompt-file ./task.md
#   ./bin/agent-dispatch.sh --role ta-go-qa-falsification --cwd $(pwd) --dry-run
#   ./bin/agent-dispatch.sh --role ta-go-builder --model qwen3-coder:30b
#
# The dispatcher walks the role's fallback chain (defined in
# .claude/agent-chains.sh). Each tier:
#   1. Preflight check (backend reachable + model available + auth).
#   2. For ollama-* tiers: acquire an mkdir-protected slot, wait up to wait_max.
#      For codex-exec / claude-native: no slot — dispatch immediately.
#   3. Dispatch. On success: write response to stdout, log served_by, exit 0.
#   4. On preflight fail, slot timeout, or non-zero dispatch exit: advance.
# All tiers exhausted → "CHAIN FAILED" to stderr, exit 1.
#
# Lock policy: only Ollama tiers (local + cloud) use lock dirs. External APIs
# (Codex, Anthropic) self-rate-limit via 429/401 responses — we listen to their
# exit codes instead of trying to predict their capacity.
#
# Locking primitive: `mkdir` is atomic on POSIX filesystems and ships on every
# Unix (no `flock(1)` dependency — macOS doesn't have it). Stale-lock cleanup
# uses PID files inside each lock dir + kill -0 liveness check.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHAINS_FILE="${REPO_ROOT}/.claude/agent-chains.sh"
PERSONA_DIR="${REPO_ROOT}/.claude/agents"
LOCK_DIR="/tmp/agent-dispatch"

ROLE=""
CWD="${PWD}"
PROMPT_FILE=""
PROMPT_STRING=""
DRY_RUN=0
MODEL_OVERRIDE=""
HELD_LOCK=""
GATE_JSON=""

usage() {
  cat >&2 <<'USAGE'
Usage: agent-dispatch.sh --role <role-name> [options]

Walks the role's fallback chain (.claude/agent-chains.sh) until one tier
succeeds. Writes the chosen backend's response to stdout, dispatch diagnostic
to stderr. Exit 0 on success (any tier), exit 1 if all tiers exhausted.

Options:
  --role <name>          Required. e.g. ta-go-builder, ta-go-qa-falsification
  --cwd <abs-path>       Working directory for the dispatched agent (default: PWD)
  --prompt <string>      Inline task prompt string. Highest precedence: wins over
                         --prompt-file and stdin. Use when the orchestrator wants
                         to avoid `echo "..." | dispatcher` (each new echo + pipe
                         triggers a fresh permission prompt; --prompt does not).
  --prompt-file <path>   Read task prompt from this file (default: stdin)
  --model <tag>          Override tier-1 model (later tiers unchanged).
                         For atomic 1-4 block builder droplets per cascade
                         methodology, the default tier-1 is already a small
                         coder model; override only when a larger model is
                         needed for an unusual case.
  --dry-run              Print the tier-1 command that would be executed; exit.
                         Skips slot acquisition so you can dry-run while the
                         system is under load.
  -h | --help            This message

The task prompt is read from stdin unless --prompt-file is given.

Stderr diagnostic format:
  [disp] tier N: backend model
  [disp]   SKIP — <reason>            (preflight, slot timeout, dispatch error)
  [disp] served_by=backend:model      (tier 1 succeeded)
  [disp] served_by=X originally_requested=Y FALLBACK   (tier N>1 succeeded)
  [disp] CHAIN FAILED: N tiers exhausted for role=...   (all tiers failed)

Stdout output format (orchestrator parses based on stderr served_by line):
  ollama-local / claude-native → Claude Code JSON envelope:
    {"type":"result", "result":"<text>", "usage":{...}, ...}
  codex-exec → raw codex stream output. Reply is the contiguous
    non-marker block before the "tokens used" footer.

Cost-reporting caveat: total_cost_usd in the JSON envelope is reported
by Claude Code based on the requested model name. For ollama-routed tiers
(ANTHROPIC_BASE_URL redirected to localhost), the actual Anthropic bill is
$0 — the local GPU served the request. Trust served_by= over total_cost_usd.
USAGE
  exit 2
}

# --- Argument parsing ------------------------------------------------------

while [[ $# -gt 0 ]]; do
  case "$1" in
    --role)         ROLE="$2";           shift 2 ;;
    --cwd)          CWD="$2";            shift 2 ;;
    --prompt)       PROMPT_STRING="$2";  shift 2 ;;
    --prompt-file)  PROMPT_FILE="$2";    shift 2 ;;
    --model)        MODEL_OVERRIDE="$2"; shift 2 ;;
    --gate)         GATE_JSON="$2";      shift 2 ;;
    --dry-run)      DRY_RUN=1;           shift ;;
    -h|--help)      usage ;;
    *)              echo "Unknown arg: $1" >&2; usage ;;
  esac
done

[[ -z "${ROLE}" ]] && { echo "Missing --role" >&2; usage; }
[[ ! -f "${CHAINS_FILE}" ]] && { echo "Chains file missing: ${CHAINS_FILE}" >&2; exit 1; }
mkdir -p "${LOCK_DIR}"

# Ensure any held lock is released on any exit path.
cleanup() {
  if [[ -n "${HELD_LOCK}" && -d "${HELD_LOCK}" ]]; then
    rm -rf "${HELD_LOCK}" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

# shellcheck source=/dev/null
source "${CHAINS_FILE}"

TIER_TABLE="$(emit_chain_for_role "${ROLE}")"
[[ -z "${TIER_TABLE}" ]] && { echo "No chain defined for role: ${ROLE}" >&2; exit 1; }

PERSONA_FILE="${PERSONA_DIR}/${ROLE}.md"
[[ ! -f "${PERSONA_FILE}" ]] && { echo "Persona file missing: ${PERSONA_FILE}" >&2; exit 1; }

# Parse YAML frontmatter from persona.
PERSONA_BODY="$(awk '
  BEGIN { state = "pre" }
  /^---$/ {
    if      (state == "pre")   { state = "in_fm"; next }
    else if (state == "in_fm") { state = "post";  next }
  }
  state == "post" { print }
' "${PERSONA_FILE}")"

TOOLS_LINE="$(awk '
  BEGIN { state = "pre" }
  /^---$/ {
    if      (state == "pre")   { state = "in_fm"; next }
    else if (state == "in_fm") { exit }
  }
  state == "in_fm" && /^tools:/ {
    sub(/^tools:[[:space:]]*/, "")
    print
    exit
  }
' "${PERSONA_FILE}")"

# Read task prompt. Precedence: --prompt (inline string) > --prompt-file > stdin.
# --prompt is the orchestrator-friendly path: avoids `echo "..." | dispatcher`
# which triggers a fresh permission prompt on every distinct compound command.
if [[ -n "${PROMPT_STRING}" ]]; then
  TASK_PROMPT="${PROMPT_STRING}"
elif [[ -n "${PROMPT_FILE}" ]]; then
  [[ ! -f "${PROMPT_FILE}" ]] && { echo "Prompt file missing: ${PROMPT_FILE}" >&2; exit 1; }
  TASK_PROMPT="$(cat "${PROMPT_FILE}")"
else
  TASK_PROMPT="$(cat)"
fi

# --- Gate contract (--gate '{"edit":[...],"writable_dirs":[...],"bash_deny":[...],"network":bool}') ---
# ONE JSON gate spec → per-backend translation (codex: execpolicy rules + -C; claude -p:
# --allowedTools(//abs) + --disallowedTools). Empty => ungated. The orchestrator owns the spec;
# this is the bin/sh proof-of-concept the sand MCP will replace.
GATE_EDIT_FILES=()
GATE_BASH_DENY=()
GATE_WRITABLE_DIRS=()
if [[ -n "${GATE_JSON}" ]]; then
  while IFS= read -r line; do [[ -n "$line" ]] && GATE_EDIT_FILES+=("$line"); done < <(printf '%s' "${GATE_JSON}" | python3 -c "import sys,json;d=json.loads(sys.stdin.read() or '{}');print(chr(10).join(x for x in d.get('edit',[]) if isinstance(x,str)))")
  while IFS= read -r line; do [[ -n "$line" ]] && GATE_BASH_DENY+=("$line"); done < <(printf '%s' "${GATE_JSON}" | python3 -c "import sys,json;d=json.loads(sys.stdin.read() or '{}');print(chr(10).join(x for x in d.get('bash_deny',[]) if isinstance(x,str)))")
  while IFS= read -r line; do [[ -n "$line" ]] && GATE_WRITABLE_DIRS+=("$line"); done < <(printf '%s' "${GATE_JSON}" | python3 -c "import sys,json;d=json.loads(sys.stdin.read() or '{}');print(chr(10).join(x for x in d.get('writable_dirs',[]) if isinstance(x,str)))")
fi

# --- Per-run audit capture (veracity + sand reference corpus) --------------
# The bin/sh model must persist the FULL trace of every dispatch so (a) an
# orchestrator can AUDIT that an agent's self-report matches what actually ran
# (no silent off-scope action), and (b) sand has real reference data to build
# from. Per tier we capture the backend's stdout (the response +, for codex,
# the tool-call stream) and stderr (codex execpolicy "Rejected(...)" lines,
# diagnostics) to .claude/agent-runs/ (gitignored — transient artifacts). The
# `-p`/ollama JSON envelope's permission_denials + tool_use and the codex
# stream are the ground truth the orchestrator checks claims against. (Built-in
# Agent-tool gate decisions live separately in .claude/hooks/ta_gate_debug.log,
# since that channel does not route through this dispatcher.)
AUDIT_DIR="${REPO_ROOT}/.claude/agent-runs"
mkdir -p "${AUDIT_DIR}" 2>/dev/null || true
AUDIT_BASE="${AUDIT_DIR}/$(date +%Y%m%d-%H%M%S)-${ROLE}-$$"

ANTI_RECURSION='

---

DISPATCH CONTEXT: You are the '"${ROLE}"' agent, dispatched via bin/agent-dispatch.sh. Execute the task below directly using YOUR role-appropriate tools (the orchestrator restricts them per the persona'"'"'s `tools:` allowlist). Do NOT call agent-dispatch.sh. Do NOT use the Agent tool to spawn other roles. Do NOT route the task elsewhere. You ARE the role. The orchestrator coordinates further dispatches.'

cd "${CWD}"

# --- Preflight per backend -------------------------------------------------

PREFLIGHT_REASON=""

preflight() {
  local backend=$1 model=$2
  PREFLIGHT_REASON=""

  case "$backend" in
    ollama-local|ollama-cloud)
      if ! curl -sf --max-time 3 http://localhost:11434/api/version >/dev/null; then
        PREFLIGHT_REASON="ollama daemon unreachable at localhost:11434"
        return 1
      fi
      if [[ "$backend" == "ollama-local" ]]; then
        if ! ollama list 2>/dev/null | awk 'NR>1 {print $1}' | grep -qx "$model"; then
          PREFLIGHT_REASON="model $model not pulled locally"
          return 1
        fi
      fi
      if [[ "$backend" == "ollama-cloud" ]]; then
        if [[ -z "${OLLAMA_API_KEY:-}" ]]; then
          PREFLIGHT_REASON="OLLAMA_API_KEY unset; cloud auth missing"
          return 1
        fi
      fi
      ;;
    codex-exec)
      if ! command -v codex >/dev/null 2>&1; then
        PREFLIGHT_REASON="codex CLI not on PATH"
        return 1
      fi
      ;;
    claude-native)
      if ! command -v claude >/dev/null 2>&1; then
        PREFLIGHT_REASON="claude CLI not on PATH"
        return 1
      fi
      ;;
    *)
      PREFLIGHT_REASON="unknown backend: $backend"
      return 1
      ;;
  esac
  return 0
}

# --- Ollama slot acquisition (mkdir-based, portable) ----------------------

is_lock_stale() {
  local lock=$1
  [[ -f "${lock}/pid" ]] || return 1   # no pid file → can't tell, assume not stale
  local pid
  pid="$(cat "${lock}/pid" 2>/dev/null || true)"
  [[ -z "$pid" ]] && return 1
  # kill -0 succeeds if the pid exists; fails if it's gone.
  if kill -0 "$pid" 2>/dev/null; then
    return 1   # still alive, not stale
  fi
  return 0     # stale
}

acquire_ollama_slot() {
  local backend=$1 slots=$2 wait_max=$3
  local i lock deadline

  [[ -z "$slots" || "$slots" -le 0 ]] && { echo ""; return 0; }

  deadline=$(($(date +%s) + wait_max))
  while (( $(date +%s) <= deadline )); do
    for ((i=1; i<=slots; i++)); do
      lock="${LOCK_DIR}/${backend}.${i}.lock"

      # Reap stale locks (process died holding the lock).
      if [[ -d "$lock" ]] && is_lock_stale "$lock"; then
        rm -rf "$lock" 2>/dev/null || true
      fi

      # Atomic claim: mkdir succeeds for exactly one process.
      if mkdir "$lock" 2>/dev/null; then
        echo $$ > "${lock}/pid"
        echo "$lock"
        return 0
      fi
    done
    sleep 0.5
  done
  return 1
}

release_ollama_slot() {
  local lock=$1
  [[ -n "$lock" && -d "$lock" ]] && rm -rf "$lock" 2>/dev/null || true
}

# --- Per-backend dispatch -------------------------------------------------

dispatch_ollama() {
  local model=$1
  # `claude -p` against a local ollama endpoint. `--bare` retired 2026-05-27
  # (see AGENT_SANDBOX_SPEC.md §12) — plain `-p` gets full default context
  # (CLAUDE.md auto-load, plugins, hooks, project .mcp.json) matching what
  # built-in Agent gets. Parity > clean-context strip.
  #
  # Per-persona tool surface is gated by `--settings <persona-settings.json>`
  # (claude code applies permissions.allow/deny natively for the `-p` path) +
  # the project-level PreToolUse hook (ta_action_gate.py) for baseline safety.
  # Per-dispatch edit-path scope is DEFERRED — see
  # EDIT_PATH_SCOPE_GATING_DEFERRED.md.
  local persona_settings="${PERSONA_DIR}/${ROLE}/settings.json"
  if [[ ! -f "${persona_settings}" ]]; then
    echo "[disp] ERROR: persona settings.json missing at ${persona_settings}" >&2
    echo "[disp]   Each persona must have <project>/.claude/agents/<persona>/settings.json with permissions.allow/deny." >&2
    return 2
  fi
  local cmd=( claude -p
    --model "${model}"
    --output-format stream-json
    --verbose
    --no-session-persistence
    --settings "${persona_settings}"
    --append-system-prompt "${PERSONA_BODY}${ANTI_RECURSION}"
  )

  if [[ "${DRY_RUN}" -eq 1 ]]; then
    printf '  TILL_PERSONA=%q ANTHROPIC_BASE_URL=http://localhost:11434 ANTHROPIC_API_KEY=ollama \\\n' "${ROLE}" >&2
    printf '  cd %q && \\\n' "${CWD}" >&2
    printf '  ' >&2
    printf '%q ' "${cmd[@]}" >&2
    printf '\n  <<< <stdin task prompt>\n' >&2
    return 0
  fi

  # ANTHROPIC_BASE_URL routes inference to local ollama. ANTHROPIC_API_KEY=ollama
  # satisfies claude code's auth layer (ollama accepts any value). No
  # CLAUDE_CODE_DISABLE_* env vars (parity with built-in: we WANT CLAUDE.md +
  # auto-memory + git-instructions auto-loaded so the persona sees the same
  # context a built-in Agent dispatch would see).
  #
  # TILL_PERSONA tells ta_action_gate.py (PreToolUse Bash hook) which persona
  # settings.json to read for permissions.deny. Empirically verified
  # 2026-05-27: claude code does NOT enforce --settings <file>'s
  # permissions.deny in headless `-p` mode without --bare; the hook is the
  # only universal per-persona enforcement layer.
  TILL_PERSONA="${ROLE}" \
  ANTHROPIC_BASE_URL="http://localhost:11434" \
  ANTHROPIC_API_KEY="ollama" \
    "${cmd[@]}" <<<"${TASK_PROMPT}"
}

dispatch_codex() {
  local model=$1 opts=$2
  local full_prompt="${PERSONA_BODY}${ANTI_RECURSION}

---

${TASK_PROMPT}"

  # -C confines writes to the gate's writable dir (editing roles) else the project cwd.
  local codex_cwd="${CWD}"
  [[ "${#GATE_WRITABLE_DIRS[@]}" -gt 0 ]] && codex_cwd="${GATE_WRITABLE_DIRS[0]}"
  # NOTE: --ignore-rules REMOVED (2026-05-25 4-way consensus). We WANT our hermetic
  # CODEX_HOME/rules/default.rules execpolicy to apply — it is the RELIABLE, OS-independent
  # git/command block (native .git-ro is geometry-/`/tmp`-dependent: sand E5, hylla T3clean vs
  # T3v3). --ignore-user-config + the hermetic CODEX_HOME still exclude the dev's global rules.
  local cmd=( codex exec
    --ephemeral
    --ignore-user-config
    --skip-git-repo-check
    -C "${codex_cwd}"
    -m "${model}"
  )
  # workspace-write sandbox is INERT in exec without an approval policy (sand E3); `-a` is not a
  # valid `codex exec` flag (sand E4) — the knob is `-c approval_policy="never"`.
  cmd+=( -c "approval_policy=\"never\"" )

  # Codex MCP injection — HERMETIC (mirrors dispatch_claude_native's
  # "--bare + explicit MCP" pattern). --ignore-user-config (set above) means
  # codex does NOT read ~/.codex/config.toml, so ALL its HOME state is
  # ignored: the conflicting hylla url= entry, the tillsyn server, HOME
  # gopls/context7, agents.md, everything. We inject ONLY what each role
  # needs inline, per the persona tool matrix (2026-05-24):
  #   ta       — always (cascade substrate).
  #   hylla    — planning + plan-qa only, READ-ONLY; NOT build-qa (just-
  #              shipped code isn't in the Hylla snapshot yet — build-qa
  #              relies on git diff + LSP/Read).
  #   context7 — always (library / tooling docs). HTTP remote; header maps
  #              to the CONTEXT7_API_KEY env var (must be exported here).
  #   gopls    — Go roles only (live Go symbol semantics).
  #   web_search — re-enabled per-run (HOME web_search="live" is ignored).
  # Approval syntax: per-tool `approval_mode = "approve"` is the form that
  # ACTUALLY pre-approves under --ephemeral (approval: never). Server-level
  # default_tools_approval_mode is documented but NOT implemented for raw
  # mcp_servers (upstream #16501) — verified empirically. Per-tool form
  # mirrors the user's working ~/.codex/config.toml.
  cmd+=( -c "web_search=\"live\"" )

  # Ignore ALL AGENTS.md instruction docs (global ~/.codex/AGENTS.md AND any
  # project AGENTS.md walked root->cwd). --ignore-user-config skips config.toml
  # but NOT AGENTS.md; codex has no dedicated disable flag, so we cap the
  # instruction-doc budget to 0 bytes. The persona body (--append via the
  # prompt) is the agent's ONLY instruction source — fully hermetic.
  cmd+=( -c "project_doc_max_bytes=0" )

  # Disable codex's BUNDLED skills (imagegen / openai-docs / plugin-creator /
  # skill-creator / skill-installer) so the agent's world is ONLY the persona +
  # the injected MCP — nothing ambient. (--ignore-user-config skips config.toml
  # but NOT the runtime-bundled skills; this knob does. Verified 2026-05-25:
  # SKILLS=NONE under this flag.)
  cmd+=( -c "skills.bundled.enabled=false" )

  local ta_tools_toml="" tool
  for tool in get update list_sections search schema create delete move init; do
    [[ -n "${ta_tools_toml}" ]] && ta_tools_toml+=","
    ta_tools_toml+="${tool}={approval_mode=\"approve\"}"
  done
  cmd+=( -c "mcp_servers.ta={command=\"ta\",args=[\"--project\",\"${CWD}\"],startup_timeout_sec=15,tools={${ta_tools_toml}}}" )

  # Hylla MCP injection (stdio). READ-ONLY tool set (excludes hylla.ingest /
  # hylla.config.refresh). Tool names are the canonical names the hylla MCP
  # server registers. SKIPPED for build-qa roles per the persona tool matrix.
  # Per-tool quoted keys required because dots inside an inline-table key
  # otherwise create nested structure.
  if [[ "${ROLE}" != *build-qa* ]]; then
    local hylla_tools_toml="" hylla_tool
    for hylla_tool in hylla.artifact.list hylla.artifact.metadata hylla.artifact.overview hylla.dql.query hylla.graph.list hylla.graph.nav hylla.node.full hylla.refs.find hylla.run.get hylla.run.list hylla.search hylla.search.keyword hylla.search.vector hylla.task.get; do
      [[ -n "${hylla_tools_toml}" ]] && hylla_tools_toml+=","
      hylla_tools_toml+="\"${hylla_tool}\"={approval_mode=\"approve\"}"
    done
    cmd+=( -c "mcp_servers.hylla={command=\"/Users/evanschultz/go/bin/hylla\",args=[\"mcp\"],startup_timeout_sec=15,tools={${hylla_tools_toml}}}" )
  fi

  # Context7 MCP injection (HTTP remote — mirrors the user's HOME
  # context7-mcp def). env_http_headers maps the CONTEXT7_API_KEY header to
  # the same-named env var, which must be exported where this dispatcher
  # runs. Injected for every codex role EXCEPT build-qa: build-qa is a
  # reading-based axis (it inspects shipped code + reads library source
  # directly), and the HTTP context7 server is a startup network call that
  # intermittently hangs codex MCP-init (SAND_E2E_PROOF §4 flagged MCP
  # injection as never-asserted-green) — so the leanest reliable MCP set
  # for build-qa codex is ta only.
  if [[ "${ROLE}" != *build-qa* ]]; then
    cmd+=( -c "mcp_servers.context7={url=\"https://mcp.context7.com/mcp\",env_http_headers={CONTEXT7_API_KEY=\"CONTEXT7_API_KEY\"},startup_timeout_sec=15}" )
  fi

  # gopls MCP injection (Go roles only, EXCEPT build-qa). gopls `mcp` indexes
  # the module at startup — a heavy MCP-init that intermittently hangs codex
  # for build-qa, which only needs to READ (not resolve live symbols). Keep
  # gopls for go planning/plan-qa; strip it from build-qa for reliable startup.
  if [[ "${ROLE}" == *-go-* && "${ROLE}" != *build-qa* ]]; then
    local gopls_tools_toml="" gopls_tool
    for gopls_tool in go_diagnostics go_file_context go_package_api go_search go_symbol_references go_workspace; do
      [[ -n "${gopls_tools_toml}" ]] && gopls_tools_toml+=","
      gopls_tools_toml+="${gopls_tool}={approval_mode=\"approve\"}"
    done
    cmd+=( -c "mcp_servers.gopls={command=\"gopls\",args=[\"mcp\"],cwd=\"${CWD}\",startup_timeout_sec=15,tools={${gopls_tools_toml}}}" )
  fi

  # Playwright MCP injection (FE roles only). @playwright/mcp is npx-cached
  # and the Playwright browsers are installed (~/Library/Caches/ms-playwright,
  # incl. the MCP's own mcp-chrome) — no extra install needed. Like the
  # context7 HTTP server, the MCP runs as a codex SUBPROCESS, not under the
  # shell --sandbox, so it launches a headless browser + reaches the live
  # Wails dev server (localhost:34917) regardless of read-only/workspace-write
  # mode. --isolated keeps each dispatch's browser profile ephemeral so
  # parallel FE dispatches don't contend on a shared user-data-dir. Tool
  # names are the @playwright/mcp browser_* set (the same tools the Claude
  # Code playwright plugin wraps). This is what lets FE qa-falsification run
  # on codex with mandatory Playwright, not only on claude-native.
  if [[ "${ROLE}" == *-fe-* ]]; then
    local pw_tools_toml="" pw_tool
    for pw_tool in browser_navigate browser_navigate_back browser_click browser_type browser_press_key browser_hover browser_select_option browser_fill_form browser_file_upload browser_handle_dialog browser_drag browser_snapshot browser_take_screenshot browser_console_messages browser_network_requests browser_evaluate browser_resize browser_wait_for browser_tabs browser_close browser_install; do
      [[ -n "${pw_tools_toml}" ]] && pw_tools_toml+=","
      pw_tools_toml+="${pw_tool}={approval_mode=\"approve\"}"
    done
    cmd+=( -c "mcp_servers.playwright={command=\"/opt/homebrew/bin/playwright-mcp\",args=[\"--headless\",\"--isolated\"],startup_timeout_sec=15,tools={${pw_tools_toml}}}" )
  fi

  if [[ -n "${opts}" ]]; then
    # shellcheck disable=SC2206  # intentional word-split on opts
    local opt_arr=( ${opts} )
    cmd+=( "${opt_arr[@]}" )
  fi

  if [[ "${DRY_RUN}" -eq 1 ]]; then
    printf '  CODEX_HOME=<hermetic tmp: only auth.json/version.json/installation_id/models_cache.json symlinked> \\\n' >&2
    printf '  ' >&2
    printf '%q ' "${cmd[@]}" >&2
    printf '\n  <<< <persona + task>\n' >&2
    return 0
  fi

  # Hermetic CODEX_HOME. ~/.codex holds ALL global surfaces codex would load:
  # skills/, rules/, hooks, plugins/, memories/, ambient-suggestions/,
  # AGENTS.md, config.toml. There is no single flag to disable skills (codex
  # issue #14316) or all hooks, so we point CODEX_HOME at a throwaway dir that
  # contains ONLY the auth + identity files (symlinked) codex needs to run.
  # Everything global is therefore ABSENT — the persona body + the -c MCP
  # injections are the agent's entire world. --ephemeral means no session
  # state is written back. (--ignore-user-config + project_doc_max_bytes=0 +
  # --ignore-rules remain as belt-and-suspenders, incl. for PROJECT-level
  # AGENTS.md/.rules that live in the repo, not under CODEX_HOME.)
  local hermetic_home rc f
  hermetic_home="$(mktemp -d "${TMPDIR:-/tmp}/codex-hermetic.XXXXXX")"
  for f in auth.json version.json installation_id models_cache.json; do
    [[ -e "${HOME}/.codex/${f}" ]] && ln -s "${HOME}/.codex/${f}" "${hermetic_home}/${f}"
  done
  # Execpolicy git/command denylist — the RELIABLE block (CreateProcess-level, geometry/OS-
  # independent). git mutations ALWAYS forbidden (orchestrator is sole committer); plus the gate's
  # non-git bash_deny patterns (e.g. "mage install", "go get", "go mod"). Loaded because we do NOT
  # pass --ignore-rules; the hermetic CODEX_HOME keeps the dev's global rules out.
  mkdir -p "${hermetic_home}/rules"
  {
    local gv
    for gv in commit push add reset rebase merge checkout branch tag stash restore cherry-pick am clean switch rm mv update-ref gc prune worktree submodule init clone fetch pull remote apply; do
      printf 'prefix_rule(pattern=["git", "%s"], decision="forbidden")\n' "${gv}"
    done
    local pat toks
    for pat in "${GATE_BASH_DENY[@]:-}"; do
      [[ -z "${pat}" ]] && continue
      case "${pat}" in git\ *|git) continue ;; esac
      toks="$(printf '%s' "${pat}" | python3 -c "import sys;print(', '.join('\"%s\"'%t for t in sys.stdin.read().split()))")"
      [[ -n "${toks}" ]] && printf 'prefix_rule(pattern=[%s], decision="forbidden")\n' "${toks}"
    done
  } > "${hermetic_home}/rules/default.rules"
  CODEX_HOME="${hermetic_home}" "${cmd[@]}" <<<"${full_prompt}" && rc=0 || rc=$?
  rm -rf "${hermetic_home}" 2>/dev/null || true
  return "${rc}"
}

dispatch_claude_native() {
  local model=$1
  # `claude -p` against the real Anthropic API. UN-RETIRED 2026-05-27 (see
  # AGENT_SANDBOX_SPEC.md §12). Earlier directive forbade `-p` on OAuth, but
  # with --bare retired, plain `-p` is the canonical headless OAuth path —
  # same flag shape as dispatch_ollama minus the ollama routing env vars.
  # Hooks fire, plugins auto-load, CLAUDE.md auto-loads, project .mcp.json
  # auto-loads. Per-persona settings.json carries permissions.allow/deny.
  # Auth: OAuth via keychain, or ANTHROPIC_API_KEY env if the caller exports
  # one (Anthropic real API key path). Claude code picks whichever it finds.
  # Per-dispatch edit-path scope is DEFERRED — see
  # EDIT_PATH_SCOPE_GATING_DEFERRED.md.
  local persona_settings="${PERSONA_DIR}/${ROLE}/settings.json"
  if [[ ! -f "${persona_settings}" ]]; then
    echo "[disp] ERROR: persona settings.json missing at ${persona_settings}" >&2
    echo "[disp]   Each persona must have <project>/.claude/agents/<persona>/settings.json with permissions.allow/deny." >&2
    return 2
  fi
  local cmd=( claude -p
    --model "${model}"
    --output-format stream-json
    --verbose
    --no-session-persistence
    --settings "${persona_settings}"
    --append-system-prompt "${PERSONA_BODY}${ANTI_RECURSION}"
  )

  if [[ "${DRY_RUN}" -eq 1 ]]; then
    printf '  TILL_PERSONA=%q \\\n' "${ROLE}" >&2
    printf '  cd %q && \\\n' "${CWD}" >&2
    printf '  ' >&2
    printf '%q ' "${cmd[@]}" >&2
    printf '\n  <<< <stdin task prompt>\n' >&2
    return 0
  fi

  # TILL_PERSONA tells ta_action_gate.py (PreToolUse Bash hook) which persona
  # settings.json to read for permissions.deny. Same env-var contract as
  # dispatch_ollama; auth differs (OAuth via keychain instead of ollama).
  TILL_PERSONA="${ROLE}" \
    "${cmd[@]}" <<<"${TASK_PROMPT}"
}

# --- Chain walk -----------------------------------------------------------

PRIMARY_BACKEND=""
PRIMARY_MODEL=""
TIER_NUM=0

while IFS='|' read -r backend model opts wait_max slots; do
  [[ -z "$backend" || "$backend" =~ ^[[:space:]]*# ]] && continue

  TIER_NUM=$((TIER_NUM + 1))

  if [[ "$TIER_NUM" -eq 1 && -n "${MODEL_OVERRIDE}" ]]; then
    model="${MODEL_OVERRIDE}"
  fi

  if [[ -z "$PRIMARY_BACKEND" ]]; then
    PRIMARY_BACKEND="$backend"
    PRIMARY_MODEL="$model"
  fi

  echo "[disp] tier ${TIER_NUM}: ${backend} ${model}" >&2

  if ! preflight "$backend" "$model"; then
    echo "[disp]   SKIP — preflight: ${PREFLIGHT_REASON}" >&2
    continue
  fi

  # Slot acquisition for Ollama tiers only. Skipped entirely in dry-run mode
  # so dry-run can run while the system is under load without hijacking a slot.
  HELD_LOCK=""
  if [[ "${DRY_RUN}" -ne 1 ]]; then
    case "$backend" in
      ollama-local|ollama-cloud)
        if ! HELD_LOCK="$(acquire_ollama_slot "$backend" "$slots" "$wait_max")"; then
          echo "[disp]   SKIP — slot timeout after ${wait_max}s (${slots} slots full)" >&2
          continue
        fi
        ;;
    esac
  fi

  # Run the dispatch, capturing the REAL exit code. `if ! cmd; then $?` is 0
  # because `!` inverts the exit code; we use `cmd && rc=0 || rc=$?` so DISPATCH_EXIT
  # holds cmd's actual exit code (and `set -e` doesn't fire because the `||` branch
  # is part of a compound conditional).
  DISPATCH_OK=1
  DISPATCH_EXIT=0
  # Capture this tier's stdout (response/stream) + stderr (tool stream, codex
  # execpolicy rejections) to the per-run audit files, then pass both through to
  # the dispatcher's real stdout/stderr so the orchestrator still receives them.
  TIER_OUT="${AUDIT_BASE}.tier${TIER_NUM}.${backend}.out"
  TIER_ERR="${AUDIT_BASE}.tier${TIER_NUM}.${backend}.err"
  case "$backend" in
    ollama-local|ollama-cloud)
      dispatch_ollama "$model" > "${TIER_OUT}" 2> "${TIER_ERR}" && DISPATCH_EXIT=0 || DISPATCH_EXIT=$?
      ;;
    codex-exec)
      dispatch_codex "$model" "$opts" > "${TIER_OUT}" 2> "${TIER_ERR}" && DISPATCH_EXIT=0 || DISPATCH_EXIT=$?
      ;;
    claude-native)
      dispatch_claude_native "$model" > "${TIER_OUT}" 2> "${TIER_ERR}" && DISPATCH_EXIT=0 || DISPATCH_EXIT=$?
      ;;
  esac
  [[ -s "${TIER_OUT}" ]] && cat "${TIER_OUT}"
  [[ -s "${TIER_ERR}" ]] && cat "${TIER_ERR}" >&2
  [[ "${DISPATCH_EXIT}" -ne 0 ]] && DISPATCH_OK=0

  release_ollama_slot "${HELD_LOCK}"
  HELD_LOCK=""

  if [[ "${DISPATCH_OK}" -eq 1 ]]; then
    printf '{"run":"%s","role":"%s","backend":"%s","model":"%s","tier":%d,"exit":0,"served_by":"%s:%s","cwd":"%s","gate":%s,"ts":"%s","stdout":"%s","stderr":"%s"}\n' \
      "$(basename "${AUDIT_BASE}")" "${ROLE}" "${backend}" "${model}" "${TIER_NUM}" "${backend}" "${model}" "${CWD}" \
      "${GATE_JSON:-null}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      "$(basename "${TIER_OUT}")" "$(basename "${TIER_ERR}")" > "${AUDIT_BASE}.meta.json" 2>/dev/null || true
    if [[ "${TIER_NUM}" -eq 1 ]]; then
      echo "[disp] served_by=${backend}:${model}" >&2
    else
      echo "[disp] served_by=${backend}:${model} originally_requested=${PRIMARY_BACKEND}:${PRIMARY_MODEL} FALLBACK" >&2
    fi
    echo "[disp] audit=${AUDIT_BASE}.*" >&2
    exit 0
  fi

  echo "[disp]   SKIP — dispatch exited ${DISPATCH_EXIT}" >&2

  [[ "${DRY_RUN}" -eq 1 ]] && exit 0
done <<<"${TIER_TABLE}"

echo "[disp] CHAIN FAILED: ${TIER_NUM} tiers exhausted for role=${ROLE}" >&2
echo "[disp] CODEX_EXHAUSTED role=${ROLE} — orchestrator should re-dispatch via the native Agent tool with subagent_type=${ROLE} model=sonnet (equal-tier Anthropic substitute). Escalate to opus ONLY on repeated sonnet failures for plan-QA-falsif slices (orch judgment, not automatic). Do NOT invoke 'claude -p' as a subprocess — that path was retired 2026-05-21 to keep claude-native billing on the subscription, not on ANTHROPIC_API_KEY." >&2
exit 1
