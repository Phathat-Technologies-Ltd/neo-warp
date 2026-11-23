"""Converts istupakov/parakeet-tdt-0.6b-v3-onnx's fp32 encoder to the fp16 encoder Core ML runs.

    pip install onnx onnxconverter-common
    python export_parakeet_fp16.py encoder-model.onnx encoder-model.fp16.onnx

`encoder-model.onnx` needs its `encoder-model.onnx.data` beside it. Inputs and outputs stay fp32.
"""

import re
import sys

import onnx
from onnxconverter_common import float16

model = onnx.load(sys.argv[1])
# The subsampling's output-length arithmetic casts the int64 lengths to float and back; converted,
# its Add mixes fp32 and fp16 and the model no longer loads.
length_path = [
    node.name
    for node in model.graph.node
    if re.fullmatch(r"/pre_encode/(Cast|Add|Div|Floor|Constant)(_\d+)?", node.name)
    and not re.fullmatch(r"/pre_encode/Constant_(9|1\d)", node.name)
]
converted = float16.convert_float_to_float16(
    model, keep_io_types=True, disable_shape_infer=True, node_block_list=length_path
)
onnx.save(converted, sys.argv[2])
