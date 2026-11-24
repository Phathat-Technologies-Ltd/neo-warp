"""Converts Parakeet TDT 0.6B v3's encoder to the compiled Core ML model the Neural Engine runs.

    pip install "nemo_toolkit[asr]" coremltools torch
    python export_parakeet_coreml.py parakeet-tdt-0.6b-v3.nemo <out dir>

Writes `<out dir>/encoder-model.fp16.mlmodelc.tar`: one compiled multifunction ML Program
(`encoder-model.fp16.mlmodelc`) with a function per input length, `frames_500`, `frames_1500`
and `frames_3000` feature frames, sharing one fp16 weight file. The Neural Engine runs only static
shapes; the app pads an input to the smallest function that fits. Each function takes
`audio_signal` (fp32 [1, 128, frames] log-mel features) and `length` (int32 [1], the unpadded
frame count) and returns `outputs` (fp32 [1, 1024, steps]) and `encoded_lengths` (int32 [1]),
like the ONNX export.

The published model (phathat/parakeet-tdt-0.6b-v3-fp16-coreml) was made from
nvidia/parakeet-tdt-0.6b-v3 revision 541d1f99c6b0c3cd0b11a95167540bb8edefd82b with
nemo_toolkit 3.0.0, torch 2.7.0, coremltools 9.0 and Xcode's coremlcompiler 3600.25.1.
"""

import os
import subprocess
import sys
import tarfile
import tempfile

import coremltools as ct
import numpy as np
import torch
from nemo.collections.asr.models import ASRModel
from nemo.collections.asr.parts.submodules import multi_head_attention

BUCKETS = (500, 1500, 3000)
NAME = "encoder-model.fp16.mlmodelc"
SUBSAMPLING = 8

nemo_path, out_dir = sys.argv[1], sys.argv[2]
encoder = ASRModel.restore_from(nemo_path, map_location="cpu").eval().encoder.eval()
pe_len = int(encoder.pos_enc.pe.shape[1])
steps = {}


def static_rel_pos_forward(self, x, cache_len=0):
    # The upstream forward slices the positional table with tensor arithmetic on the input length,
    # which coremltools can't convert; each trace has a fixed length, so slice with Python ints.
    n = steps["n"] + cache_len
    center = pe_len // 2 + 1
    pos_emb = self.pe[:, center - n : center + n - 1].detach().clone()
    if self.dropout_emb:
        pos_emb = self.dropout_emb(pos_emb)
    return self.dropout(x), pos_emb


multi_head_attention.RelPositionalEncoding.forward = static_rel_pos_forward


class Encoder(torch.nn.Module):
    def __init__(self, encoder):
        super().__init__()
        self.encoder = encoder

    def forward(self, audio_signal, length):
        encoded, encoded_len = self.encoder(audio_signal=audio_signal, length=length.long())
        return encoded, encoded_len.int()


wrapper = Encoder(encoder).eval()
with tempfile.TemporaryDirectory() as tmp:
    descriptor = ct.utils.MultiFunctionDescriptor()
    for frames in BUCKETS:
        steps["n"] = -(-frames // SUBSAMPLING)
        example = (torch.randn(1, 128, frames), torch.tensor([frames], dtype=torch.int32))
        with torch.no_grad():
            traced = torch.jit.trace(wrapper, example, check_trace=False)
        mlmodel = ct.convert(
            traced,
            inputs=[
                ct.TensorType(name="audio_signal", shape=(1, 128, frames), dtype=np.float32),
                ct.TensorType(name="length", shape=(1,), dtype=np.int32),
            ],
            outputs=[
                ct.TensorType(name="outputs", dtype=np.float32),
                ct.TensorType(name="encoded_lengths", dtype=np.int32),
            ],
            convert_to="mlprogram",
            compute_precision=ct.precision.FLOAT16,
            minimum_deployment_target=ct.target.macOS15,
            compute_units=ct.ComputeUnit.CPU_ONLY,
        )
        package = os.path.join(tmp, f"encoder-{frames}.mlpackage")
        mlmodel.save(package)
        descriptor.add_function(package, "main", f"frames_{frames}")
    descriptor.default_function_name = f"frames_{BUCKETS[0]}"
    unlabeled = os.path.join(tmp, "unlabeled.mlpackage")
    ct.utils.save_multifunction(descriptor, unlabeled)
    mlmodel = ct.models.MLModel(unlabeled, skip_model_load=True)
    multifunction = os.path.join(tmp, "encoder-model.fp16.mlpackage")
    mlmodel.author = "NVIDIA (Parakeet TDT 0.6B v3); Core ML conversion by phathat"
    mlmodel.license = "CC BY 4.0"
    mlmodel.short_description = "Parakeet TDT 0.6B v3 FastConformer encoder, fp16"
    mlmodel.save(multifunction)
    compiled = os.path.join(tmp, "compiled")
    subprocess.run(
        ["xcrun", "coremlcompiler", "compile", multifunction, compiled,
         "--platform", "macos", "--deployment-target", "15.0"],
        check=True,
    )
    os.replace(os.path.join(compiled, "encoder-model.fp16.mlmodelc"), os.path.join(tmp, NAME))
    archive = os.path.join(out_dir, f"{NAME}.tar")
    with tarfile.open(archive, "w", format=tarfile.USTAR_FORMAT) as tar:
        def reset(info):
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 0
            return info

        tar.add(os.path.join(tmp, NAME), arcname=NAME, filter=reset)
    print(archive)
