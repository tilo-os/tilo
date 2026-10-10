import os,resource,sys
resource.setrlimit(resource.RLIMIT_AS,(1073741824,1073741824))
resource.setrlimit(resource.RLIMIT_CORE,(0,0))
os.execv(sys.argv[1],sys.argv[1:])
