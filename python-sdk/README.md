# loom

Python client library for the loom browser-automation daemon.

```python
import loom

with loom.Session.create() as session:
    session.navigate("https://example.com")
```
