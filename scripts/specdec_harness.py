#!/usr/bin/env python3
"""specdec_harness.py — measure spec-dec acceptance rate + TG across prompt regimes.

Hits an OpenAI-compatible llama-server (assumes spec dec is enabled there).
Parses /metrics for accept stats; falls back to server-log grep if /metrics absent.

Boo's prompt set: structured / code / factual / reasoning / creative / long-form
technical / token-dense JSON / translation. Reasoning + long-form-technical
(prompts 4 + 6) are the workload-relevant ones; if those clear 65% acceptance
the 15 t/s path is real, sub-55% means pivot to KV quant + lower-bit weights.

Usage:
    python3 specdec_harness.py [--endpoint URL] [--model NAME] [--out csv]
"""
import argparse, csv, json, sys, time, urllib.request, urllib.error

PROMPTS = [
    ("01_structured",
     "List the first 30 prime numbers, comma-separated, no commentary."),
    ("02_code_rust",
     "Write a Rust function that takes a &[u8] slice and returns the SHA-256 hash as a hex String. No external crates beyond sha2."),
    ("03_factual",
     "Explain how Vulkan descriptor sets differ from Vulkan push constants. Three paragraphs."),
    ("04_reasoning",
     "A train leaves Chicago at 60mph heading east. Another leaves NYC at 80mph heading west. Distance is 790 miles. When and where do they meet? Show work."),
    ("05_creative",
     "Write a 200-word noir scene set in a server room during a power outage."),
    ("06_long_technical",
     "Explain load-time weight repacking for Q4_K quantized weights in a Vulkan compute shader context. What's the memory layout transformation and why does it help on shared-memory iGPUs?"),
    ("07_json_dense",
     "Generate a JSON object representing a fictional CI/CD pipeline config with 5 stages, each with name, image, script array, and artifacts. Valid JSON only, no commentary."),
    ("08_translation",
     "Translate this to formal Spanish: 'The bandwidth ceiling is real, but speculative decoding with a same-family draft model can break through it on iGPU inference.'"),
]

def post(url, body, timeout=180):
    data = json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())

def log_tail(path, since_offset):
    """Return (new_text, new_offset). Empty if file missing."""
    import os
    try:
        size = os.path.getsize(path)
        if size < since_offset:
            since_offset = 0  # log rotated
        with open(path, "rb") as f:
            f.seek(since_offset)
            data = f.read()
        return data.decode(errors="replace"), size
    except (FileNotFoundError, IOError):
        return "", since_offset

def parse_accept(text):
    """Parse llama-server lines like:
       draft acceptance rate = 0.85714 (   30 accepted /    35 generated)
       Returns (accepted, generated) summed across all matches in text, or (0,0)."""
    import re
    pat = re.compile(r"draft acceptance rate\s*=\s*[\d.]+\s*\(\s*(\d+)\s*accepted\s*/\s*(\d+)\s*generated\s*\)")
    acc = drafted = 0
    for m in pat.finditer(text):
        acc += int(m.group(1))
        drafted += int(m.group(2))
    return acc, drafted

def parse_eval_tps(text):
    """Sum eval-time tokens-per-second weighted by token count.
       Pattern: 'eval time = X ms / Y tokens ( Z ms per token, W tokens per second)'
       Excludes 'prompt eval time'. Returns weighted-mean TG."""
    import re
    pat = re.compile(r"^\s+eval time\s*=\s*([\d.]+)\s*ms\s*/\s*(\d+)\s*tokens.*?([\d.]+) tokens per second", re.MULTILINE)
    total_tok = 0
    total_ms = 0.0
    for m in pat.finditer(text):
        if "prompt eval time" in text[max(0, m.start()-30):m.start()]:
            continue
        ms = float(m.group(1))
        tok = int(m.group(2))
        total_tok += tok
        total_ms += ms
    return (total_tok / (total_ms / 1000.0)) if total_ms > 0 else 0.0

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--endpoint", default="http://127.0.0.1:8080")
    ap.add_argument("--model", default=None, help="model id, auto-detected if omitted")
    ap.add_argument("--out", default="/tmp/specdec_harness.csv")
    ap.add_argument("--max-tokens", type=int, default=512)
    ap.add_argument("--server-log", default="/tmp/8b_specdec.log",
                    help="server log path (used to parse accept rate + per-prompt TG)")
    args = ap.parse_args()

    base = args.endpoint.rstrip("/")
    if not args.model:
        try:
            with urllib.request.urlopen(f"{base}/v1/models", timeout=5) as r:
                d = json.loads(r.read())
                args.model = d["data"][0]["id"]
        except Exception as e:
            print(f"failed to auto-detect model: {e}", file=sys.stderr)
            sys.exit(1)

    print(f"endpoint={base} model={args.model} out={args.out}", flush=True)

    import os
    log_offset = os.path.getsize(args.server_log) if os.path.exists(args.server_log) else 0

    rows = []
    for tag, prompt in PROMPTS:
        body = {
            "model": args.model,
            "messages": [{"role": "user", "content": "/no_think\n\n" + prompt}],
            "temperature": 0.0,
            "max_tokens": args.max_tokens,
        }
        t0 = time.time()
        try:
            r = post(f"{base}/v1/chat/completions", body)
            elapsed = time.time() - t0
            usage = r.get("usage", {}) or {}
            completion_tokens = usage.get("completion_tokens") or 0

            time.sleep(0.3)  # let server flush log
            new_log, log_offset = log_tail(args.server_log, log_offset)
            accepted, drafted = parse_accept(new_log)
            tg_log = parse_eval_tps(new_log)
            tg = tg_log if tg_log > 0 else (completion_tokens / elapsed if elapsed > 0 else 0)
            accept_rate = (accepted / drafted) if drafted > 0 else None

            row = {
                "tag": tag,
                "completion_tokens": completion_tokens,
                "wall_s": round(elapsed, 2),
                "tg_tps": round(tg, 2),
                "accept_rate": round(accept_rate, 3) if accept_rate is not None else None,
                "drafted": drafted or None,
                "accepted": accepted or None,
            }
            rows.append(row)
            print(f"[{tag}] tokens={completion_tokens} tg={row['tg_tps']} acc={row['accept_rate']} ({accepted}/{drafted})", flush=True)
        except Exception as e:
            print(f"[{tag}] ERROR: {e}", file=sys.stderr, flush=True)
            rows.append({"tag": tag, "error": str(e)})

    with open(args.out, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["tag", "completion_tokens", "wall_s", "tg_tps", "accept_rate", "drafted", "accepted", "error"])
        w.writeheader()
        for r in rows:
            w.writerow(r)
    print(f"\nWrote {args.out}", flush=True)

    # Decision summary per Boo's threshold
    workload_rows = [r for r in rows if r.get("tag") in ("04_reasoning", "06_long_technical") and r.get("accept_rate") is not None]
    if workload_rows:
        avg = sum(r["accept_rate"] for r in workload_rows) / len(workload_rows)
        verdict = "GREEN: 15 t/s path real" if avg > 0.65 else "AMBER: marginal" if avg > 0.55 else "RED: pivot to KV quant + lower bits"
        print(f"\nWorkload-relevant accept (#04 + #06): {avg:.2%} -> {verdict}")

if __name__ == "__main__":
    main()
