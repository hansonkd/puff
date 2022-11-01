from puff.http import global_http_client
from asgiref.sync import async_to_sync

http_client = global_http_client()


def test_req_post_json():
    resp = http_client.post(
        "https://httpbin.org/post",
        headers={"my-header": b"greenlet-header", "my-header-2": b"abc"},
        json={"hello": "greenlet"},
    )
    assert resp.headers["content-type"] == b"application/json"
    resp_data = resp.json()
    assert resp_data["json"]["hello"] == "greenlet"
    assert resp_data["headers"]["My-Header"] == "greenlet-header"
    assert resp_data["headers"]["My-Header-2"] == "abc"


def test_req_post_form():
    resp = http_client.post(
        "https://httpbin.org/post", data={"hello": "form", "form-field-2": "form 😀"}
    )
    assert resp.header("content-type") == b"application/json"
    resp_data = resp.json()
    assert resp_data["form"]["hello"] == "form"
    assert resp_data["form"]["form-field-2"] == "form 😀"


def test_req_post_multi():
    resp = http_client.post(
        "https://httpbin.org/post",
        data={"hello": "multi"},
        files={"file1": "hello-world", "file2": ("hiworld.txt", b"yo world")},
    )
    resp_data = resp.json()
    assert resp_data["form"]["hello"] == "multi"
    assert resp_data["files"]["file1"] == "hello-world"
    assert resp_data["files"]["file2"] == "yo world"


async def do_http_request():
    resp = await http_client.post(
        "https://httpbin.org/post",
        headers={"my-header": b"async-header", "my-header-2": b"abc"},
        json={"hello": "async"},
    )
    return await resp.json()


def test_req_post_async():
    resp_data = async_to_sync(do_http_request)()
    assert resp_data["json"]["hello"] == "async"
    assert resp_data["headers"]["My-Header"] == "async-header"
    assert resp_data["headers"]["My-Header-2"] == "abc"
