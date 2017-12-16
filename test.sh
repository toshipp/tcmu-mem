#!/bin/sh

gen_wwn() {
    # from rtslib-fb utils.py
    rand=$(cat /dev/urandom | tr -cd '[:xdigit:]' | dd status=none bs=9 count=1 | tr '[:upper:]' '[:lower:]')
    echo -n "naa.5001405$rand"
}

setup() {
    modprobe tcm_loop
    modprobe target_core_user

    wwn=$(gen_wwn | tee /tmp/rand_wwn)

    # create loopback
    cd /sys/kernel/config/target
    tpgt=loopback/$wwn/tpgt_0
    mkdir -p $tpgt
    gen_wwn | tee $tpgt/nexus >/dev/null
    mkdir -p $tpgt/lun/lun_0

    # create user backstore
    mkdir -p core/user_0/mydisk
#    echo -n dev_size=1073741824 | tee core/user_0/mydisk/control >/dev/null
    echo -n dev_config=my/vol | tee core/user_0/mydisk/control >/dev/null
    echo -n 1 | tee core/user_0/mydisk/enable >/dev/null
}

start() {
    wwn=$(cat /tmp/rand_wwn)

    # connect
    ln -s /sys/kernel/config/target/core/user_0/mydisk/ \
       /sys/kernel/config/target/loopback/$wwn/tpgt_0/lun/lun_0/mydisk
}

shutdown() {
    wwn=$(cat /tmp/rand_wwn)

    # disconnect
    rm /sys/kernel/config/target/loopback/$wwn/tpgt_0/lun/lun_0/mydisk

    cd /sys/kernel/config/target
    # delete backstore
    rmdir core/user_0/mydisk
    rmdir core/user_0

    # delete loopback
    rmdir loopback/$wwn/tpgt_0/lun/lun_0
    rmdir loopback/$wwn/tpgt_0
    rmdir loopback/$wwn
}

eval $1
