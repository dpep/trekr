class Proxy < BasicObject
  def initialize(target)
    @target = target
  end

  def method_missing(name, *args, &block)
    @target.__send__(name, *args, &block)
  end
end

class Wrapper < Proxy
  def honk
    :honk
  end
end

class Settings
  def self.method_missing(...)
    instance.public_send(...)
  end
end

class Record
  def method_missing(name, *args)
    name.to_s.start_with?("find_by_") ? nil : super
  end
end

class Engine
  def start
    :vroom
  end

  def stop
    :halt
  end

  def idle
    :idle
  end
end

Wrapper.new(Engine.new).start
Settings.stop
Record.new.idle
