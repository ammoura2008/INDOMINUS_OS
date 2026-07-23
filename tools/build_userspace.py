#!/usr/bin/env python3
"""Build userspace binaries and pack them into the initrd.

Usage:
    python tools/build_userspace.py

Builds the init and shell binaries for the custom x86_64-indominus target,
then creates the initrd image with them.
"""

import subprocess
import os
import sys
import shutil

WORKSPACE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
USERSPACE_DIR = os.path.join(WORKSPACE, "userspace")
TARGET_JSON = os.path.join(USERSPACE_DIR, "x86_64-indominus.json")
ROOTFS_DIR = os.path.join(USERSPACE_DIR, "rootfs")
INITRD_OUTPUT = os.path.join(WORKSPACE, "indo-kernel", "initrd.img")
BUILD_INITRD = os.path.join(WORKSPACE, "tools", "build_initrd.py")

# Packages to build (name, crate path)
PACKAGES = [
    ("init", "userspace/init"),
    ("indosh", "userspace/shell"),
]


def run(cmd, cwd=WORKSPACE, env=None):
    """Run a command and return success/failure."""
    print(f"  $ {' '.join(cmd)}")
    result = subprocess.run(
        cmd,
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=120,
        env=env,
    )
    if result.returncode != 0:
        print(f"  FAILED: {result.stderr[:500]}")
        return False
    return True


def build_package(name, crate_path):
    """Build a userspace package with cargo."""
    print(f"[BUILD] Building {name}...")
    
    # Ensure build directory exists
    build_dir = os.path.join(ROOTFS_DIR, "bin")
    os.makedirs(build_dir, exist_ok=True)

    target = TARGET_JSON
    target_dir = os.path.join(WORKSPACE, "target", "userspace")

    cmd = [
        "cargo", "build",
        "--target", target,
        "--target-dir", target_dir,
        "--release",
        "-p", name,
    ]

    if not run(cmd):
        return False

    # Copy the binary to rootfs/bin/
    # The binary name might have the target triple suffix
    binary_names = [name, f"{name}-x86_64-indominus"]
    src_dir = os.path.join(target_dir, "x86_64-indominus", "release")
    
    found = False
    for binary_name in binary_names:
        src = os.path.join(src_dir, binary_name)
        if os.path.exists(src):
            dst = os.path.join(build_dir, binary_name)
            shutil.copy2(src, dst)
            print(f"  -> {dst}")
            found = True
            break
    
    if not found:
        # List what's in the directory
        if os.path.exists(src_dir):
            files = os.listdir(src_dir)
            print(f"  Available files in {src_dir}: {files}")
        else:
            print(f"  Source directory not found: {src_dir}")
        return False

    return True


def build_initrd():
    """Build the initrd from rootfs."""
    print("[BUILD] Building initrd...")
    cmd = [
        sys.executable, BUILD_INITRD,
        "--input", ROOTFS_DIR,
        "--output", INITRD_OUTPUT,
    ]
    return run(cmd)


def main():
    print("=" * 60)
    print("INDOMINUS OS — Userspace Build")
    print("=" * 60)

    # Check if target JSON exists
    if not os.path.exists(TARGET_JSON):
        print(f"Error: target JSON not found: {TARGET_JSON}")
        sys.exit(1)

    # Ensure rootfs structure
    os.makedirs(os.path.join(ROOTFS_DIR, "bin"), exist_ok=True)
    os.makedirs(os.path.join(ROOTFS_DIR, "etc"), exist_ok=True)

    # Build each package
    for name, crate_path in PACKAGES:
        if not build_package(name, crate_path):
            print(f"[BUILD] Failed to build {name}")
            sys.exit(1)

    # Build initrd
    if not build_initrd():
        print("[BUILD] Failed to build initrd")
        sys.exit(1)

    print("\n[BUILD] Userspace build complete!")


if __name__ == "__main__":
    main()
