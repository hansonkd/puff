from puff.redis import global_redis, named_client


def test_redis_incr():
    global_redis.set("my-key", b"0")
    result = global_redis.incr("my-key", 1)
    assert result == 1
    result = global_redis.incr("my-key", 3)
    assert result == 4
    result = global_redis.get("my-key")
    assert result == b"4"


def test_redis_mset():
    global_redis.mset({"key-1": "value-1", "key-2": "value-2"})
    assert global_redis.mget(["key-1", "key-2"]) == [b"value-1", b"value-2"]


def test_redis_lpop():
    assert global_redis.lpop("my-list") == []
    assert global_redis.rpush("my-list", "hi")
    assert global_redis.lpop("my-list") == [b"hi"]
    assert global_redis.rpush("my-list", "hi")
    assert global_redis.lpop("my-list", 2) == [b"hi"]
    assert global_redis.rpush("my-list", "hi2")
    assert global_redis.rpush("my-list", "hi3")
    assert global_redis.lpop("my-list", 3) == [b"hi2", b"hi3"]


alt_redis = named_client("altredis")


def test_redis_alt():
    alt_redis.set("my-key", b"0")
    result = alt_redis.incr("my-key", 1)
    assert result == 1
