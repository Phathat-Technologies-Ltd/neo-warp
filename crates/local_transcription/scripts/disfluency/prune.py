"""Prunes a BERT checkpoint's WordPiece vocabulary to the tokens English conversational and
terminal text uses, keeping every one-character piece so any word still tokenizes."""
import collections
import csv
import gzip
import json
import sys

import torch
from transformers import AutoModelForTokenClassification, BertTokenizerFast

BASE, DL, TRAIN, OUT = sys.argv[1:5]
tok = BertTokenizerFast.from_pretrained(BASE)
vocab = tok.get_vocab()
inv = {i: t for t, i in vocab.items()}

texts = []
with gzip.open(f"{DL}/oasst_prompts.jsonl.gz", "rt") as f:
    for line in f:
        m = json.loads(line)
        if m.get("lang") == "en":
            texts.append(m["text"])
with open(f"{DL}/spc_train.csv", newline="") as f:
    for row in csv.DictReader(f):
        texts.append(row["Best Generated Conversation"])
for line in open(TRAIN):
    texts.append(" ".join(json.loads(line)["words"]))

counts = collections.Counter()
for i in range(0, len(texts), 2000):
    for ids in tok(texts[i:i + 2000], add_special_tokens=False)["input_ids"]:
        counts.update(ids)

keep = []
for i in range(len(inv)):
    t = inv[i]
    core = t[2:] if t.startswith("##") else t
    if t.startswith("[") and t.endswith("]") and not t.startswith("[unused"):
        keep.append(i)
    elif len(core) == 1 or counts[i] >= 3:
        keep.append(i)
print("kept", len(keep), "of", len(inv))

model = AutoModelForTokenClassification.from_pretrained(BASE, num_labels=2)
emb = model.bert.embeddings.word_embeddings
new = torch.nn.Embedding(len(keep), emb.weight.shape[1], padding_idx=0)
new.weight.data = emb.weight.data[keep].clone()
model.bert.embeddings.word_embeddings = new
model.config.vocab_size = len(keep)
model.config.pad_token_id = 0
model.save_pretrained(OUT)
with open(f"{OUT}/vocab.txt", "w") as f:
    for i in keep:
        f.write(inv[i] + "\n")
BertTokenizerFast(vocab_file=f"{OUT}/vocab.txt", do_lower_case=True).save_pretrained(OUT)
