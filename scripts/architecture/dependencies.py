from pathlib import Path
import tomllib

from scripts.architecture.budgets import checked_path, relative_path


POLICY_PATH = "architecture/dependencies.toml"
KINDS = ("dependencies", "build-dependencies", "dev-dependencies")


def load(path: Path) -> dict:
    return tomllib.loads(path.read_text(encoding="utf-8-sig"))


def dependency_entries(manifest: dict, workspace: dict):
    for section in [manifest, *manifest.get("target", {}).values()]:
        for kind in KINDS:
            for alias, original in section.get(kind, {}).items():
                specification = {"version": original} if isinstance(original, str) else original
                inherited = specification.get("workspace", False)
                if inherited:
                    if alias not in workspace.get("dependencies", {}):
                        raise ValueError(f"unresolved workspace dependency: {alias}")
                    base = workspace["dependencies"][alias]
                    base = {"version": base} if isinstance(base, str) else base
                    specification = dict(base, **specification)
                yield kind, alias, specification, inherited


def cycles(graph: dict[str, set[str]]) -> list[str]:
    visited = set()
    active = []
    errors = []

    def visit(node):
        if node in active:
            start = active.index(node)
            errors.append("production dependency cycle: " + " -> ".join(active[start:] + [node]))
            return
        if node in visited:
            return
        active.append(node)
        for target in sorted(graph[node]):
            visit(target)
        active.pop()
        visited.add(node)

    for node in sorted(graph):
        visit(node)
    return errors


def check(root: Path, policy: dict, source_roots=None, scanned_paths=None):
    workspace = load(root / "Cargo.toml")["workspace"]
    members = workspace["members"]
    if not isinstance(members, list) or not members or len(set(members)) != len(members):
        raise ValueError("workspace architecture needs nonempty, unique members")
    if any(any(character in member for character in "*?[") for member in members):
        raise ValueError("workspace members must be explicit for architecture classification")
    for member in members:
        relative_path(member)
        checked_path(root, member)
    if workspace.get("exclude"):
        raise ValueError("workspace exclusions require explicit architecture support")
    rules = policy.get("crates", {})
    errors = []
    if policy.get("version") != 1:
        raise ValueError("unsupported dependency policy version")
    if policy.keys() - {"version", "crates", "renderer_packages"}:
        raise ValueError("unknown dependency policy field")
    for member in sorted(set(members) ^ rules.keys()):
        errors.append(f"{member}: workspace membership and architecture classification differ")
    manifests = {member: load(root / member / "Cargo.toml") for member in members}
    names = {manifest["package"]["name"]: member for member, manifest in manifests.items()}
    paths = {(root / member).resolve(): member for member in members}
    if len(names) != len(members):
        raise ValueError("duplicate workspace package names")
    graph = {member: set() for member in members}
    source_roots = source_roots if source_roots is not None else members
    forbidden = set(policy.get("renderer_packages", []))
    for member, manifest in manifests.items():
        rule = rules.get(member)
        if rule is None:
            continue
        if rule.keys() - {*KINDS, "layer", "zero_production_dependencies"}:
            errors.append(f"{member}: unknown dependency rule field")
        if rule.get("layer") not in {"core", "application", "lab", "hook"}:
            errors.append(f"{member}: invalid architecture layer")
            continue
        targets = [manifest.get("lib", {})]
        targets.extend(target for kind in ("bin", "test", "bench", "example") for target in manifest.get(kind, []))
        build = manifest["package"].get("build")
        if isinstance(build, str):
            targets.append({"path": build})
        for target_spec in targets:
            if "path" in target_spec:
                target_path = checked_path(root, member + "/" + target_spec["path"])
                covered = any(target_path.is_relative_to(checked_path(root, source)) for source in source_roots)
                if scanned_paths is not None:
                    covered = target_path.relative_to(root.resolve()).as_posix() in scanned_paths
                if not target_path.is_file() or not covered or target_path.suffix != ".rs":
                    errors.append(f"{member}: invalid custom source path {target_spec['path']}")
        for kind in KINDS:
            for target in rule.get(kind, []):
                if target not in manifests or target == member:
                    errors.append(f"{member}: invalid {kind} allowance: {target}")
        for kind, alias, specification, inherited in dependency_entries(manifest, workspace):
            name = specification.get("package", alias)
            target = None
            if "path" in specification:
                directory = root if inherited else root / member
                resolved = (directory / specification["path"]).resolve()
                local_target = paths.get(resolved)
                if local_target is None:
                    errors.append(f"{member}: unclassified path dependency {alias}")
                    continue
                if names.get(name) != local_target:
                    errors.append(f"{member}: dependency package/path mismatch for {alias}")
                    continue
                target = local_target
            if rule.get("zero_production_dependencies", False) and kind != "dev-dependencies":
                errors.append(f"{member}: zero-dependency contract forbids {kind} {alias}")
            if rule["layer"] in {"core", "hook"} and kind != "dev-dependencies" and name in forbidden:
                errors.append(f"{member}: renderer dependency {name} is not allowed in core")
            if target is not None:
                if target not in rule.get(kind, []):
                    errors.append(f"{member}: forbidden {kind} edge to {target}")
                if kind != "dev-dependencies":
                    target_layer = rules.get(target, {}).get("layer")
                    if ((rule["layer"] == "core" and target_layer != "core")
                            or (rule["layer"] == "application" and target_layer == "lab")):
                        errors.append(f"{member}: forbidden production layer direction to {target}")
                    graph[member].add(target)
    return errors + cycles(graph), members
