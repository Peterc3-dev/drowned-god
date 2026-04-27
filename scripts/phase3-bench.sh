#!/usr/bin/env bash
# phase3-bench.sh — sweep 8B + 27B (Q4_K_M, Q3_K_M) in headless mode and dump
# a single artifact dir under ~/phase3-results/$(date).
# Run AFTER `dg-mode brick`, ideally over SSH so you can read the output.
set -euo pipefail

ART=~/phase3-results/$(date +%Y%m%d-%H%M%S)
mkdir -p "$ART"
echo "[*] artifacts → $ART"

LLAMA=/usr/local/bin/llama-server
EVAL=/tmp/bfcl_eval/run_eval.py
HARNESS=/home/raz/projects/drowned-god/scripts/specdec_harness.py

# Snapshot system state going into bench
free -h > "$ART/00_pre_mem.txt"
uptime  > "$ART/00_pre_uptime.txt"
systemctl get-default > "$ART/00_target.txt"
systemctl is-active sddm 2>&1 > "$ART/00_sddm_status.txt" || true

start_server() {
  local name=$1 ; shift
  echo "[bench] starting $name"
  pkill -x llama-server 2>/dev/null || true
  sleep 4
  nohup "$LLAMA" "$@" > "$ART/srv_$name.log" 2>&1 &
  local pid=$!
  echo $pid > "$ART/srv_$name.pid"
  # wait for /health
  for i in {1..40}; do
    if curl -sS --max-time 1 http://127.0.0.1:8080/health 2>/dev/null | grep -q ok; then
      echo "[bench] $name ready (PID $pid, took ${i}s)"
      return 0
    fi
    sleep 1
  done
  echo "[bench] $name failed to come up"; tail -30 "$ART/srv_$name.log"; return 1
}

bench_one() {
  local name=$1 model=$2
  echo
  echo "============================================================"
  echo "[bench] $name (model=$model)"
  echo "============================================================"
  free -h > "$ART/${name}_mem_before.txt"
  # patch BFCL eval model field for this run
  sed -i "s|\"model\": \".*\"|\"model\": \"$model\"|" "$EVAL"
  python3 "$EVAL" 2>&1 | tee "$ART/${name}_bfcl.log"
  python3 "$HARNESS" --max-tokens 256 --out "$ART/${name}_harness.csv" --server-log "$ART/srv_${name}.log" 2>&1 | tee "$ART/${name}_harness.log"
  free -h > "$ART/${name}_mem_after.txt"
}

echo "[*] === run 1: Qwen3-8B + Qwen3-1.7B draft (spec dec) ==="
start_server qwen8b_specdec \
  -m /home/raz/models/Qwen3-8B-Q4_K_M/Qwen3-8B-Q4_K_M.gguf \
  -md /home/raz/models/Qwen3-1.7B-Q4_K_M/Qwen3-1.7B-Q4_K_M.gguf \
  -ngl 99 -ngld 99 -t 8 \
  --jinja --reasoning-format deepseek \
  --port 8080 --host 0.0.0.0 \
  -c 16384 --cache-ram 1024 --reasoning-budget 0 \
  -ctk q8_0 -ctv q8_0 --flash-attn 1 \
  -ctkd q8_0 -ctvd q8_0 \
  --draft-max 16 --draft-min 4 --draft-p-min 0.7 \
  --metrics
bench_one qwen8b_specdec "Qwen3-8B-Q4_K_M.gguf"

echo "[*] === run 2: Qwen3.6-27B Q4_K_M (no spec dec — SSM wall) ==="
start_server qwen36_q4 \
  -m /home/raz/models/Qwen3.6-27B-Q4_K_M/Qwen3.6-27B-Q4_K_M-00001-of-00001.gguf \
  -ngl 99 -t 8 \
  --jinja --reasoning-format deepseek \
  --port 8080 --host 0.0.0.0 \
  -c 8192 --cache-ram 1024 --reasoning-budget 0 \
  -ctk q8_0 -ctv q8_0 --flash-attn 1 \
  --metrics
bench_one qwen36_q4 "Qwen3.6-27B-Q4_K_M-00001-of-00001.gguf"

echo "[*] === run 3: Qwen3.6-27B Q3_K_M (K-quant fast path test) ==="
start_server qwen36_q3km \
  -m /home/raz/models/Qwen3.6-27B-Q3_K_M/Qwen_Qwen3.6-27B-Q3_K_M.gguf \
  -ngl 99 -t 8 \
  --jinja --reasoning-format deepseek \
  --port 8080 --host 0.0.0.0 \
  -c 8192 --cache-ram 1024 --reasoning-budget 0 \
  -ctk q8_0 -ctv q8_0 --flash-attn 1 \
  --metrics
bench_one qwen36_q3km "Qwen_Qwen3.6-27B-Q3_K_M.gguf"

# leave 8B+specdec running so the bot stays alive
echo "[*] restoring default endpoint (8B+specdec) for openclaw/QwenLo"
start_server qwen8b_default \
  -m /home/raz/models/Qwen3-8B-Q4_K_M/Qwen3-8B-Q4_K_M.gguf \
  -md /home/raz/models/Qwen3-1.7B-Q4_K_M/Qwen3-1.7B-Q4_K_M.gguf \
  -ngl 99 -ngld 99 -t 8 \
  --jinja --reasoning-format deepseek \
  --port 8080 --host 0.0.0.0 \
  -c 16384 --cache-ram 1024 --reasoning-budget 0 \
  -ctk q8_0 -ctv q8_0 --flash-attn 1 \
  -ctkd q8_0 -ctvd q8_0 \
  --draft-max 16 --draft-min 4 --draft-p-min 0.7 \
  --metrics

echo
echo "[*] DONE. artifacts in $ART"
echo "[*] summary:"
for f in "$ART"/*_harness.csv; do
  name=$(basename "$f" _harness.csv)
  echo "--- $name ---"
  awk -F, 'NR==1{next} {acc+=($5!=""); n++; tg+=$4; if($5!=""){a+=$5; ac++}} END{if(n) printf "  prompts=%d  avg TG=%.1f t/s  avg accept=%.1f%%\n", n, tg/n, (ac?100*a/ac:0)}' "$f"
done
