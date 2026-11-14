#! /usr/bin/env python
# %%
from __future__ import annotations
from pathlib import Path
import subprocess
from tqdm import tqdm

# %%
if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(
        description="Use probe-rs to dump the flash memory of a device to a binary file."
    )
    parser.add_argument(
        "--outputfile", "-o",
        type=Path,
        default=Path("flashdump"),
        help="Output directory (default: flashdump)"
    )
    parser.add_argument(
        "--size", "-s",
        type=int,
        default=2 * 1024 * 1024,
        help="Size of the flash memory to dump in bytes (default: 2MB)"
    )
    parser.add_argument(
        "--base", "-b",
        type=lambda x: int(x, base=16),
        default=0x10000000,
        help="Base address of the flash memory (in hex, default: 0x10000000)"
    )
    # parser.add_argument(
    #     'logfile',
    #     type=Path,
    #     help="Path to the log file to extract offset from"
    # )
    args = parser.parse_args()
    
    size: int = args.size
    base: int = args.base
    offset: int = 0
    
    output_path: Path = args.outputfile
    output_path.parent.mkdir(parents=True, exist_ok=True)

    pbar = tqdm(total=size - offset, unit="B",
                unit_scale=True, desc="Dumping flash memory",
                dynamic_ncols=True)
    success = True
    with output_path.open("wb") as f:
        try:
            while offset < size:
                chunk = min(1024, size - offset)
                pbar.desc = f'Reading {chunk} bytes [{offset:#04x}]'
                cmd = [
                    "probe-rs",
                    "read",
                    "b8",
                    f'{base + offset:#x}',
                    f'{chunk}',
                ]
                result = subprocess.run(cmd, capture_output=True)
                if result.returncode != 0:
                    print("Error executing probe-rs:")
                    print(result.stderr.decode())
                    success = False
                    break
                data = result.stdout
                data = data.strip().split()
                if len(data) != chunk:
                    print(
                        f"Warning: Expected {chunk} bytes, but got {len(data)} bytes")
                bytes = bytearray(int(byte, 16) for byte in data)
                f.write(bytes)
                pbar.update(len(bytes))
                offset += chunk
        except KeyboardInterrupt:
            print("Dumping interrupted by user")
        except Exception as e:
            print(f"An error occurred: {e}")
            success = False
    pbar.close()
    if success:
        print(f"Flash memory dumped to {output_path}")
    else:
        print("Failed to dump flash memory")
        output_path.unlink(missing_ok=True)
