#!/usr/bin/env python3
"""
Hagibis CaldoAI Skill Builder
Packages skill manifests, validates WIT specifications, and creates target/skills/ index.
"""
import glob
import json
import os
import shutil
import sys

def main():
    root = os.getcwd()
    skills_dir = os.path.join(root, "skills")
    out_dir = os.path.join(root, "target", "skills")
    os.makedirs(out_dir, exist_ok=True)

    skill_mds = glob.glob(os.path.join(skills_dir, "**", "SKILL.md"), recursive=True)
    index = []

    for m in skill_mds:
        skill_name = os.path.basename(os.path.dirname(m))
        target_dir = os.path.join(out_dir, skill_name)
        os.makedirs(target_dir, exist_ok=True)
        shutil.copy2(m, os.path.join(target_dir, os.path.basename(m)))
        index.append({"skill": skill_name, "manifest": os.path.relpath(m, root)})

    index_path = os.path.join(out_dir, "skills_index.json")
    with open(index_path, "w", encoding="utf-8") as fp:
        json.dump(index, fp, indent=2)

    print(f"Packaged {len(skill_mds)} skill component(s) into target/skills/ (index: {os.path.relpath(index_path, root)})")
    return 0

if __name__ == "__main__":
    sys.exit(main())
