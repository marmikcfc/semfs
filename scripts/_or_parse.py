#!/usr/bin/env python3
"""Parse the OpenRouter /logs RSC payload (stdin) into readable messages."""
import sys, json, re

raw = sys.stdin.read()
obj = None
for line in raw.splitlines():
    m = re.match(r'^\w+:(\{.*\})\s*$', line)
    if not m:
        continue
    try:
        d = json.loads(m.group(1))
    except Exception:
        continue
    if isinstance(d, dict) and d.get('__kind') == 'OK':
        obj = d['data']
        break
    if isinstance(d, dict) and d.get('__kind') == 'ERR':
        print("ERROR from OpenRouter:", json.dumps(d.get('error'), indent=1))
        sys.exit(3)

if obj is None:
    print("No OK payload found. Raw head:\n", raw[:600])
    sys.exit(4)


def text_of(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        out = []
        for p in content:
            if isinstance(p, dict):
                out.append(p.get('text') or p.get('content') or json.dumps(p)[:300])
            else:
                out.append(str(p))
        return "\n".join(out)
    return json.dumps(content)[:600]


inp = obj.get('input', {})
msgs = inp.get('messages', [])
print(f"=== INPUT: {len(msgs)} message(s) ===")
for i, m in enumerate(msgs):
    print(f"\n--- [{i}] role={m.get('role')} ---")
    print(text_of(m.get('content')))
if inp.get('tools'):
    print("\n=== tools:", [t.get('function', {}).get('name') for t in inp['tools']])

out = obj.get('output', {})
if out:
    print("\n=== OUTPUT ===")
    comp = out.get('completion')
    if comp is not None:
        print(comp)
    rr = out.get('rawRequest', {})
    if rr:
        print("\nmodel:", rr.get('model'), "| max_tokens:", rr.get('max_tokens'),
              "| temp:", rr.get('temperature'))
