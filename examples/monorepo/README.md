# {{ name | title_case }}

Services:
{% for s in services %}- {{ s.name }} on port {{ s.port }}
{% endfor %}