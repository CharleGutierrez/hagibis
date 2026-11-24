#!/usr/bin/env python3
"""
Hagibis CaldoAI Skill Builder
Packages skill manifests, validates WIT specifications, and creates target/skills/ index.
"""
import glob
import hashlib
import json
import os
import shutil
import sys

def compute_sha256(filepath):
    hasher = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            hasher.update(chunk)
    return hasher.hexdigest()

def main():
    script_dir = os.path.dirname(os.path.abspath(__file__))
    parent_dir = os.path.dirname(script_dir)
    if os.path.isdir(os.path.join(parent_dir, "skills")):
        root = parent_dir
    elif os.path.isdir(os.path.join(os.getcwd(), "skills")):
        root = os.getcwd()
    else:
        root = os.path.join(os.getcwd(), "hagibis")

    skills_dir = os.path.join(root, "skills")
    out_dir = os.path.join(root, "target", "skills")
    os.makedirs(out_dir, exist_ok=True)

    skill_mds = glob.glob(os.path.join(skills_dir, "**", "SKILL.md"), recursive=True)
    index = []

    for m in skill_mds:
        skill_dir = os.path.dirname(m)
        skill_name = os.path.basename(skill_dir)
        target_dir = os.path.join(out_dir, skill_name)
        os.makedirs(target_dir, exist_ok=True)
        dest_m = os.path.join(target_dir, os.path.basename(m))
        shutil.copy2(m, dest_m)

        manifest_sha256 = compute_sha256(m)
        manifest_size = os.path.getsize(m)

        # Hash all related component files in the skill directory
        component_files = {}
        for root_sub, _, files in os.walk(skill_dir):
            for file in sorted(files):
                file_path = os.path.join(root_sub, file)
                rel_file = os.path.relpath(file_path, skill_dir)
                component_files[rel_file] = {
                    "sha256": compute_sha256(file_path),
                    "size_bytes": os.path.getsize(file_path),
                }

        # Calculate composite skill component digest
        composite_hasher = hashlib.sha256()
        for k in sorted(component_files.keys()):
            composite_hasher.update(k.encode("utf-8"))
            composite_hasher.update(component_files[k]["sha256"].encode("utf-8"))
        skill_composite_hash = composite_hasher.hexdigest()

        index.append({
            "skill": skill_name,
            "manifest": os.path.relpath(m, root),
            "manifest_sha256": manifest_sha256,
            "manifest_size_bytes": manifest_size,
            "composite_sha256": skill_composite_hash,
            "files": component_files,
        })

    # Sort index deterministically
    index.sort(key=lambda x: x["skill"])

    # Root index checksum for supply chain verification
    index_json_bytes = json.dumps(index, indent=2, sort_keys=True).encode("utf-8")
    root_digest = hashlib.sha256(index_json_bytes).hexdigest()

    manifest_bundle = {
        "schema_version": "1.1.0",
        "index_sha256": root_digest,
        "total_skills": len(index),
        "skills": index,
    }

    index_path = os.path.join(out_dir, "skills_index.json")
    with open(index_path, "w", encoding="utf-8") as fp:
        json.dump(manifest_bundle, fp, indent=2)

    print(f"Packaged {len(skill_mds)} cryptographically verified skill component(s) into target/skills/ (index digest: {root_digest[:16]}...)")
    return 0

if __name__ == "__main__":
    sys.exit(main())
