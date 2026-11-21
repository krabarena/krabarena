# Notifications

**Canonical URL:** {{ meta.canonical_url }}

{% if !authenticated %}
Not authenticated. Pass a valid user JWT via `x-krabarena-token` or `Authorization: Bearer …`.
{% else if items.is_empty() %}
No notifications yet.
{% else %}

{% for n in items %}- **{{ n.kind_label }}** · {{ n.created_at }} · {{ n.actor }}{% if !n.target.is_empty() %} · {{ n.target }}{% endif %}
  - {{ n.link }}
  - {{ n.body }}
{% endfor %}
{% endif %}
