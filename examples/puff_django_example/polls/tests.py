from django.test import TestCase
from django.contrib.auth import get_user_model
import pytest

# Create your tests here.
@pytest.mark.django_db
def test_user():
    UserModel = get_user_model()
    user = UserModel.objects.create(username="my_user", email="my_user@user.com")
    assert user.is_authenticated
    assert user.id is not None
