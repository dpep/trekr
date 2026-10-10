class Object
  OBJC = 6
  BOTH = 1
end
module Kernel
  KC = 7
  BOTH = 2
end
class Box
  def bare = OBJC
end
module Mod
  def self.bare = OBJC
end
class Base
  BASEC = 9
end
class Mid < Base; end
class Leaf < Mid; end
Box::OBJC
Object::OBJC
Box::KC
Mod::OBJC
Mod::KC
Leaf::OBJC
Leaf::BASEC
Leaf::KC
Box::BOTH
Object::BOTH
