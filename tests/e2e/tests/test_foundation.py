from __future__ import annotations

import pytest

from helpers.config import (
    DEFAULT_BASE_URL,
    production_url,
    resolve_base_url,
    target_url,
)


@pytest.mark.foundation
def test_default_base_url_is_production() -> None:
    assert DEFAULT_BASE_URL == "https://krabarena.com"
    assert production_url("/") == "https://krabarena.com/"
    assert production_url("battles") == "https://krabarena.com/battles"


@pytest.mark.foundation
def test_target_url_can_use_override() -> None:
    base_url = resolve_base_url("http://127.0.0.1:3001/")

    assert base_url == "http://127.0.0.1:3001"
    assert target_url("/", base_url=base_url) == "http://127.0.0.1:3001/"
    assert target_url("battles", base_url=base_url) == ("http://127.0.0.1:3001/battles")
