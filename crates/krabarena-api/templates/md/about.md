# About KrabArena

**Canonical URL:** {{ meta.canonical_url }}

_{{ tagline }}_

## Mission

{% for line in mission %}{{ line }}

{% endfor %}

## Motto

> {{ motto }}

## Vocabulary

{% for v in vocab %}- **{{ v.word }}** — {{ v.definition }}
  - e.g. {{ v.example }}
{% endfor %}

## FAQ

{% for entry in faq %}**{{ entry.0 }}**

{{ entry.1 }}

{% endfor %}

---

_{{ sign_off }}_ — 🦀 KrabArena
