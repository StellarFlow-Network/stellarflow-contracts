import json
from typing import Any, Dict, List, Optional, Set, Union


_HASHABLE_REDUNDANT = {None, "", False, 0}


def _is_redundant(value: Any) -> bool:
    if value in _HASHABLE_REDUNDANT:
        return True
    if isinstance(value, (dict, list)) and not value:
        return True
    return False


def strip_redundant_fields(
    obj: Union[Dict[str, Any], List[Any]],
    redundant_values: Optional[Set[Any]] = None,
) -> Union[Dict[str, Any], List[Any]]:
    if redundant_values is None:
        redundant_values = _HASHABLE_REDUNDANT

    if isinstance(obj, dict):
        cleaned: Dict[str, Any] = {}
        for key, value in obj.items():
            if isinstance(value, dict):
                stripped = strip_redundant_fields(value, redundant_values)
                cleaned[key] = stripped
                continue
            if isinstance(value, list):
                stripped = strip_redundant_fields(value, redundant_values)
                cleaned[key] = stripped
                continue
            if value in redundant_values:
                continue
            if value is None and None not in redundant_values:
                cleaned[key] = value
                continue
            cleaned[key] = value
        return cleaned

    if isinstance(obj, list):
        cleaned: List[Any] = []
        for item in obj:
            if isinstance(item, (dict, list)):
                cleaned.append(strip_redundant_fields(item, redundant_values))
            elif item not in redundant_values:
                cleaned.append(item)
        return cleaned

    return obj


def _strip_empty_containers(obj: Any) -> Any:
    if isinstance(obj, dict):
        cleaned: Dict[str, Any] = {}
        for key, value in obj.items():
            stripped = _strip_empty_containers(value)
            if isinstance(stripped, (dict, list)) and not stripped:
                continue
            cleaned[key] = stripped
        return cleaned

    if isinstance(obj, list):
        cleaned: List[Any] = []
        for item in obj:
            stripped = _strip_empty_containers(item)
            if isinstance(stripped, (dict, list)) and not stripped:
                continue
            cleaned.append(stripped)
        return cleaned

    return obj


def compact_json(
    obj: Union[str, Any],
    strip_redundant: bool = True,
    redundant_values: Optional[Set[Any]] = None,
    strip_empty_containers: bool = True,
    sort_keys: bool = False,
    separators: tuple = (",", ":"),
) -> str:
    if isinstance(obj, str):
        try:
            parsed = json.loads(obj)
        except (json.JSONDecodeError, ValueError):
            return obj.strip()
        obj = parsed

    if strip_redundant:
        obj = strip_redundant_fields(obj, redundant_values)

    if strip_empty_containers:
        obj = _strip_empty_containers(obj)

    return json.dumps(
        obj,
        separators=separators,
        sort_keys=sort_keys,
        ensure_ascii=True,
    )


def compact_json_bytes(
    obj: Union[str, Any],
    strip_redundant: bool = True,
    redundant_values: Optional[Set[Any]] = None,
    strip_empty_containers: bool = True,
    sort_keys: bool = False,
    separators: tuple = (",", ":"),
) -> bytes:
    compacted = compact_json(
        obj,
        strip_redundant=strip_redundant,
        redundant_values=redundant_values,
        strip_empty_containers=strip_empty_containers,
        sort_keys=sort_keys,
        separators=separators,
    )
    return compacted.encode("utf-8")