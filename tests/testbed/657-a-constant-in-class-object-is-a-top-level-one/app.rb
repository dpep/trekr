TOP = 1
class Object
  OBJC = 6
end
module Kernel
  KC = 7
end
class Box
  def bare = OBJC
  def top = TOP
end
module Mod
  def self.bare = OBJC
  def self.top = TOP
  def self.kc = KC
end
Object::TOP
::TOP
::OBJC
Object::OBJC
Box::OBJC
Box::TOP
OBJC
::KC
