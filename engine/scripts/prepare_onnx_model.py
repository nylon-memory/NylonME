"""剥离 ONNX 模型的 value_info（tract 推理引擎前置处理）。

tract 的类型推导无法调和 value_info 里的符号形状（batch_size/sequence_length）
与调用方钉死的输入形状——「Impossible to unify Sym(batch_size) with Val(1)」。
fp32 导出通常不带 value_info，量化导出（optimum）会带，故量化模型必须剥一次。

用法: python scripts/prepare_onnx_model.py <in.onnx> <out.onnx>
"""
import sys

import onnx


def main() -> None:
    src, dst = sys.argv[1], sys.argv[2]
    model = onnx.load(src)
    del model.graph.value_info[:]
    onnx.save(model, dst)
    print(f"{dst}: value_info stripped")


if __name__ == "__main__":
    main()