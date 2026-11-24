import json
import random
import sys
import time

import torch
from transformers import AutoModelForTokenClassification, AutoTokenizer

BASE, TRAIN, VAL, OUT = sys.argv[1:5]
EPOCHS = float(sys.argv[5]) if len(sys.argv) > 5 else 2
LR = float(sys.argv[6]) if len(sys.argv) > 6 else 1e-4
MAXLEN = 64
BS = 64
dev = "mps" if torch.backends.mps.is_available() else "cpu"
torch.manual_seed(0)
random.seed(0)

tok = AutoTokenizer.from_pretrained(BASE)
model = AutoModelForTokenClassification.from_pretrained(BASE, num_labels=2).to(dev)


def load(p):
    return [json.loads(l) for l in open(p)]


def encode(rows):
    enc = tok([r["words"] for r in rows], is_split_into_words=True, truncation=True,
              max_length=MAXLEN, padding=True, return_tensors="pt")
    labels = torch.full(enc["input_ids"].shape, -100)
    for b, r in enumerate(rows):
        prev = None
        for t, w in enumerate(enc.word_ids(b)):
            if w is not None and w != prev:
                labels[b, t] = r["labels"][w]
            prev = w
    return enc, labels


train, val = load(TRAIN), load(VAL)
steps = int(len(train) / BS * EPOCHS)
opt = torch.optim.AdamW(model.parameters(), lr=LR, weight_decay=0.01)
sched = torch.optim.lr_scheduler.OneCycleLR(opt, max_lr=LR, total_steps=steps, pct_start=0.06,
                                            anneal_strategy="linear")


def evaluate():
    model.eval()
    tp = fp = fn = 0
    exact = 0
    with torch.no_grad():
        for i in range(0, len(val), 256):
            rows = val[i:i + 256]
            enc, labels = encode(rows)
            logits = model(**{k: v.to(dev) for k, v in enc.items()}).logits.cpu()
            pred = logits.argmax(-1)
            m = labels != -100
            tp += ((pred == 1) & (labels == 1) & m).sum().item()
            fp += ((pred == 1) & (labels == 0) & m).sum().item()
            fn += ((pred == 0) & (labels == 1) & m).sum().item()
            exact += sum(((pred[b] == labels[b]) | ~m[b]).all().item() for b in range(len(rows)))
    model.train()
    p = tp / max(1, tp + fp)
    r = tp / max(1, tp + fn)
    return p, r, 2 * p * r / max(1e-9, p + r), exact / len(val)


model.train()
step = 0
t0 = time.time()
while step < steps:
    random.shuffle(train)
    for i in range(0, len(train) - BS + 1, BS):
        if step >= steps:
            break
        enc, labels = encode(train[i:i + BS])
        out = model(**{k: v.to(dev) for k, v in enc.items()}, labels=labels.to(dev))
        out.loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        opt.step()
        sched.step()
        opt.zero_grad()
        step += 1
        if step % 500 == 0:
            print(step, steps, round(out.loss.item(), 4), [round(x, 4) for x in evaluate()],
                  round(time.time() - t0), flush=True)
print("final", [round(x, 4) for x in evaluate()], flush=True)
model.save_pretrained(OUT)
tok.save_pretrained(OUT)
