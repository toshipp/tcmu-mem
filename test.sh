#!/bin/sh

setup() {
    modprobe tcm_loop
    modprobe target_core_user

    # create loopback
    cd /sys/kernel/config/target
    mkdir -p loopback/naa.5000000000000000/tpgt_1/lun/lun_0
    echo -n naa.5000000100000000 | tee loopback/naa.5000000000000000/tpgt_1/nexus >/dev/null

    # create user backstore
    mkdir -p core/user_1/mydisk
    echo -n dev_size=1073741824 | tee core/user_1/mydisk/control >/dev/null
    echo -n dev_config=my/vol | tee core/user_1/mydisk/control >/dev/null
    echo -n 1 | tee core/user_1/mydisk/enable >/dev/null
}

start() {
    # connect
    ln -s /sys/kernel/config/target/core/user_1/mydisk/ \
       /sys/kernel/config/target/loopback/naa.5000000000000000/tpgt_1/lun/lun_0/mydisk
}

shutdown() {
    # disconnect
    rm /sys/kernel/config/target/loopback/naa.5000000000000000/tpgt_1/lun/lun_0/mydisk

    cd /sys/kernel/config/target
    # delete backstore
    rmdir core/user_1/mydisk

    # delete loopback
    rmdir loopback/naa.5000000000000000/tpgt_1/lun/lun_0
    rmdir loopback/naa.5000000000000000/tpgt_1/lun
    rmdir loopback/naa.5000000000000000/tpgt_1
    rmdir loopback/naa.5000000000000000
}

eval $1
