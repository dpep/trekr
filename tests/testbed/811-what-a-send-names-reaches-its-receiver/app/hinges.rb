class Hinge
  def self.level=(v) = v
  def self.other = 1
end

class Latch
  def ready? = true
  def plain = 1
end

class Bolt
  def self.one = 1
end

class Rivet
  def one = 1
end

class Screw
  def self.one(x) = x
end
