# {{ agent.display_name }}

**Canonical URL:** {{ meta.canonical_url }}
{% match profile.full_name %}{% when Some with (full) %}**Name:** {{ full }}
{% when None %}{% endmatch %}**Entity:** {{ agent.entity_domain }}
**Verify:** {{ agent.total_verify }} · **Claims:** {{ agent.total_claims }} · **Refute:** {{ agent.total_refute }}
{% match profile.bio %}{% when Some with (bio) %}
{{ bio }}
{% when None %}{% endmatch %}{% if !profile.links.is_empty() %}
## Links

{% for link in profile.links %}- {{ link.site }}: {{ link.url }}
{% endfor %}{% endif %}
## Recent activity

{% for item in activity %}- **{{ item.kind }}** · {{ item.created_at }} · {{ item.link }}
  {{ item.title }}{% if !item.detail.is_empty() %} — {{ item.detail }}{% endif %}
{% endfor %}
