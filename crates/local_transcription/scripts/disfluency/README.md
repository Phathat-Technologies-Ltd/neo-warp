# Disfluency tagger

`models/disfluency/disfluency-bert-mini.int8.onnx` is a word-level KEEP/DELETE tagger: Google's
BERT-mini (`google/bert_uncased_L-4_H-256_A-4`, Apache-2.0, revision
387825ce42dbb39b87911cdf8e383ee3b25184f8) with its vocabulary pruned to the 19,214 pieces English
conversational and terminal text uses, fine-tuned on synthetic disfluencies injected into
permissively licensed clean text:

- OpenAssistant oasst1 prompter messages (Apache-2.0), revision fdf72ae0827c1cda404aff25b6603abec9e3399b
- Google Synthetic-Persona-Chat (CC-BY-4.0), revision a520ad7f999ca7e6dfdc25fed9f5070bf6f87b42
- hand-written terminal and agent request templates in `gen.py`

No Switchboard or other non-commercial data is used.

```sh
pip install torch transformers onnx onnxruntime
# dl/ holds 2023-04-12_oasst_prompts.messages.jsonl.gz as oasst_prompts.jsonl.gz and
# Synthetic-Persona-Chat_train.csv as spc_train.csv
python gen.py dl train.jsonl 31 250000 && python gen.py dl val.jsonl 77 3000
python prune.py google/bert_uncased_L-4_H-256_A-4 dl train.jsonl base
python train.py base train.jsonl val.jsonl tagger 3 2e-4
python export.py tagger out   # out/model.int8.onnx; vocab.txt is tagger/vocab.txt
```

The shipped vocabulary was pruned on data from an earlier revision of `gen.py` (fewer templates),
so a rerun yields a nearly identical but not byte-identical `vocab.txt`.

Evaluate with `cargo run --release -p local_transcription --features disfluency --example
disfluency_eval -- testdata/disfluency_eval.tsv`.
