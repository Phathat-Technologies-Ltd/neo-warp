"""Exports a token-classification checkpoint to ONNX (fp32 + dynamic int8)."""
import sys

import torch
from onnxruntime.quantization import QuantType, quantize_dynamic
from transformers import AutoModelForTokenClassification

SRC, OUT = sys.argv[1], sys.argv[2]
model = AutoModelForTokenClassification.from_pretrained(SRC).eval()
model.config.return_dict = False


class Wrap(torch.nn.Module):
    def __init__(self, m):
        super().__init__()
        self.m = m

    def forward(self, input_ids, attention_mask, token_type_ids):
        return self.m(input_ids=input_ids, attention_mask=attention_mask,
                      token_type_ids=token_type_ids)[0]


ids = torch.ones(1, 16, dtype=torch.long)
torch.onnx.export(Wrap(model), (ids, ids, torch.zeros_like(ids)), f"{OUT}/model.fp32.onnx",
                  input_names=["input_ids", "attention_mask", "token_type_ids"],
                  output_names=["logits"], opset_version=17, dynamo=False,
                  dynamic_axes={n: {0: "batch", 1: "seq"} for n in
                                ["input_ids", "attention_mask", "token_type_ids", "logits"]})
quantize_dynamic(f"{OUT}/model.fp32.onnx", f"{OUT}/model.int8.onnx", weight_type=QuantType.QInt8)
