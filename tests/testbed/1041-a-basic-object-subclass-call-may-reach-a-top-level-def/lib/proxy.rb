class Proxy < BasicObject
  def initialize(t) = @t = t

  def method_missing(name, *args)
    @t.__send__(name, *args)
  end

  def respond_to_missing?(*) = true
end

Proxy.new(Object.new).forwarded
