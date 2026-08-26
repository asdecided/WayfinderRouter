#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf '%s\n' \
    "usage: WAYFINDER_LIVE_PROVIDER_SMOKE=1 tools/hosted-provider-live-smoke.sh --provider PROVIDER --model CONFIGURED_ID [--router-url URL]" >&2
}

provider=""
model_id=""
router_url="http://127.0.0.1:8088"
while (( $# > 0 )); do
  case "$1" in
    --provider)
      [[ $# -ge 2 && -z "$provider" ]] || { usage; exit 2; }
      provider="$2"
      shift 2
      ;;
    --model)
      [[ $# -ge 2 && -z "$model_id" ]] || { usage; exit 2; }
      model_id="$2"
      shift 2
      ;;
    --router-url)
      [[ $# -ge 2 ]] || { usage; exit 2; }
      router_url="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done

[[ "${WAYFINDER_LIVE_PROVIDER_SMOKE:-0}" == "1" ]] || {
  printf '%s\n' "Live provider smoke is disabled; set WAYFINDER_LIVE_PROVIDER_SMOKE=1 after reviewing the fixed requests and spend boundary." >&2
  exit 2
}
case "$provider" in
  openai|gemini|openrouter|groq|deepseek|together|fireworks|cerebras|xai|mistral|anthropic) ;;
  *)
    printf 'Unsupported provider evidence label: %s\n' "$provider" >&2
    exit 2
    ;;
esac
[[ "$model_id" =~ ^[A-Za-z0-9._-]{1,64}$ ]] || {
  printf '%s\n' "--model must be the 1-64 character configured Wayfinder destination id, not the upstream model name." >&2
  exit 2
}
router_url="${router_url%/}"
[[ "$router_url" =~ ^http://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]{1,5})?$ ]] || {
  printf '%s\n' "--router-url must be an explicit loopback HTTP origin." >&2
  exit 2
}
if [[ "$router_url" =~ :([0-9]{1,5})$ ]] \
  && (( 10#${BASH_REMATCH[1]} < 1 || 10#${BASH_REMATCH[1]} > 65535 )); then
  printf '%s\n' "--router-url port must be between 1 and 65535." >&2
  exit 2
fi
for command_name in curl jq awk grep mktemp date; do
  command -v "$command_name" >/dev/null 2>&1 || {
    printf 'Live provider smoke requires %s.\n' "$command_name" >&2
    exit 2
  }
done

smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/wayfinder-provider-live.XXXXXX")"
cleanup() {
  rm -rf -- "$smoke_root"
}
trap cleanup EXIT INT TERM
chmod 0700 "$smoke_root"

curl_headers=(
  --header "content-type: application/json"
  --header "accept: application/json"
)
if [[ -n "${WAYFINDER_ROUTER_VIRTUAL_KEY:-}" ]]; then
  [[ "$WAYFINDER_ROUTER_VIRTUAL_KEY" != *$'\n'* && "$WAYFINDER_ROUTER_VIRTUAL_KEY" != *$'\r'* ]] || {
    printf '%s\n' "WAYFINDER_ROUTER_VIRTUAL_KEY must be a single line." >&2
    exit 2
  }
  auth_header="$smoke_root/authorization.header"
  umask 077
  printf 'Authorization: Bearer %s\n' "$WAYFINDER_ROUTER_VIRTUAL_KEY" > "$auth_header"
  curl_headers+=(--header "@$auth_header")
fi

write_request() {
  local kind="$1"
  local output="$2"
  case "$kind" in
    buffered)
      jq -n --arg model "$model_id" '{
        model: $model,
        messages: [{role: "user", content: "Return one short greeting for a transport verification."}],
        max_tokens: 32,
        temperature: 0,
        stream: false
      }' > "$output"
      ;;
    streaming)
      jq -n --arg model "$model_id" '{
        model: $model,
        messages: [{role: "user", content: "Return one short greeting for a streaming transport verification."}],
        max_tokens: 32,
        temperature: 0,
        stream: true,
        stream_options: {include_usage: true}
      }' > "$output"
      ;;
    tool)
      jq -n --arg model "$model_id" '{
        model: $model,
        messages: [{role: "user", content: "Call wayfinder_smoke exactly once with an empty object and do not answer in text."}],
        max_tokens: 32,
        temperature: 0,
        stream: false,
        tools: [{
          type: "function",
          function: {
            name: "wayfinder_smoke",
            description: "Return deterministic compatibility evidence.",
            parameters: {type: "object", properties: {}, additionalProperties: false}
          }
        }],
        tool_choice: {type: "function", function: {name: "wayfinder_smoke"}}
      }' > "$output"
      ;;
    *) return 2 ;;
  esac
  chmod 0600 "$output"
}

request() {
  local name="$1"
  local body="$2"
  local response="$smoke_root/$name.response"
  local headers="$smoke_root/$name.headers"
  local status
  status="$(curl \
    --silent \
    --show-error \
    --max-time 90 \
    --dump-header "$headers" \
    --output "$response" \
    --write-out '%{http_code}' \
    "${curl_headers[@]}" \
    --data-binary "@$body" \
    "$router_url/v1/chat/completions")"
  if [[ "$status" != "200" ]]; then
    printf '%s request returned HTTP %s; response body is retained only until this process exits.\n' "$name" "$status" >&2
    return 1
  fi
  local served_by
  served_by="$(awk 'tolower($1) == "x-wayfinder-router-served-by:" {gsub("\\r", "", $2); print $2}' "$headers" | tail -n 1)"
  if [[ "$served_by" != "$model_id" ]]; then
    printf '%s request was served by %s instead of %s.\n' "$name" "${served_by:-<missing>}" "$model_id" >&2
    return 1
  fi
}

buffered_request="$smoke_root/buffered.request"
write_request buffered "$buffered_request"
request buffered "$buffered_request"
jq -e '
  (.choices[0].message.content | type == "string" and length > 0)
  and (.usage.prompt_tokens | type == "number")
  and (.usage.completion_tokens | type == "number")
' "$smoke_root/buffered.response" >/dev/null

streaming_request="$smoke_root/streaming.request"
write_request streaming "$streaming_request"
request streaming "$streaming_request"
grep -F 'data: [DONE]' "$smoke_root/streaming.response" >/dev/null
awk '/^data: / && $0 !~ /\[DONE\]/ {sub(/^data: /, ""); sub(/\r$/, ""); print}' \
  "$smoke_root/streaming.response" \
  | jq -s -e '
      any(.[]; (.choices[0].delta.content? | type == "string" and length > 0))
      and any(.[]; (.usage.prompt_tokens? | type == "number"))
    ' >/dev/null

tool_request="$smoke_root/tool.request"
write_request tool "$tool_request"
request tool "$tool_request"
jq -e '
  .choices[0].message.tool_calls[0].function as $function
  | $function.name == "wayfinder_smoke"
    and (($function.arguments | fromjson) == {})
    and (.usage.prompt_tokens | type == "number")
    and (.usage.completion_tokens | type == "number")
' "$smoke_root/tool.response" >/dev/null

jq -n \
  --arg schema_version "wf-provider-live-v1" \
  --arg checked_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg provider "$provider" \
  --arg destination "$model_id" \
  '{
    schema_version: $schema_version,
    checked_at: $checked_at,
    provider: $provider,
    configured_destination: $destination,
    buffered_text: "passed",
    streaming_text_usage: "passed",
    forced_function_call: "passed",
    request_count: 3,
    prompt_content: "fixed-public-smoke-only",
    automatic_changed: false
  }'
