from django.core.management import ManagementUtility


def get_management_utility(sys_args):
    return ManagementUtility(sys_args)


def get_management_utility_help(sys_args):
    return get_management_utility(sys_args).help()


def get_management_utility_execute(sys_args):
    util = get_management_utility(sys_args)
    return util.execute()
