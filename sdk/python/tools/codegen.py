"""Regenerate the gRPC stubs under src/nylon_sdk/_gen from the proto.

Usage (from the repo root):

    python sdk/python/tools/codegen.py

Requires grpcio-tools. The generated grpc stub imports
``from nylon.v1 import memory_pb2``; this script rewrites it to a
relative import so the package stays self-contained.
"""

import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[3]
OUT = REPO / "sdk" / "python" / "src" / "nylon_sdk" / "_gen"


def main() -> int:
    from grpc_tools import protoc

    rc = protoc.main(
        [
            "protoc",
            "-I",
            str(REPO / "proto"),
            f"--python_out={OUT}",
            f"--grpc_python_out={OUT}",
            f"--pyi_out={OUT}",
            "nylon/v1/memory.proto",
        ]
    )
    if rc != 0:
        return rc

    for pkg_init in (
        OUT / "__init__.py",
        OUT / "nylon" / "__init__.py",
        OUT / "nylon" / "v1" / "__init__.py",
    ):
        pkg_init.touch(exist_ok=True)

    grpc_file = OUT / "nylon" / "v1" / "memory_pb2_grpc.py"
    text = grpc_file.read_text(encoding="utf-8")
    patched = text.replace(
        "from nylon.v1 import memory_pb2 as nylon_dot_v1_dot_memory__pb2",
        "from . import memory_pb2 as nylon_dot_v1_dot_memory__pb2",
    )
    if patched == text:
        print("warning: import patch pattern not found", file=sys.stderr)
        return 1
    grpc_file.write_text(patched, encoding="utf-8")
    print("codegen ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())