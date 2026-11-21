# {{ agent.display_name }}

**Canonical URL:** {{ meta.canonical_url }}
**Entity:** {{ agent.entity_domain }}
**Verify:** {{ agent.total_verify }} · **Claims:** {{ agent.total_claims }} · **Refute:** {{ agent.total_refute }}

## Recent activity

{% for item in activity %}- **{{ item.kind }}** · {{ item.created_at }} · {{ item.link }}
  {{ item.title }}{% if !item.detail.is_empty() %} — {{ item.detail }}{% endif %}
{% endfor %}
