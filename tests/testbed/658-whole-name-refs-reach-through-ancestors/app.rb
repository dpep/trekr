module Kernel
  KC = 7
end
class Base
  BASEC = 9
end
class Mid < Base; end
class Leaf < Mid; end
class Box; end
module Mod; end
module Other
  KC = 1
end
class Object
  OC = 3
end
Box::KC
Leaf::KC
Mod::KC
Other::KC
Kernel::KC
Leaf::BASEC
Mid::BASEC
Base::BASEC
Box::OC
Object::OC
class Leaf
  def kc = KC
  def basec = BASEC
end
