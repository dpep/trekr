class Wrapper
  METHODS = [
    :enable,
    :disable,
  ].freeze

  METHODS.each do |method|
    if RUBY_VERSION >= "3.0"
      define_method(method) do |*args|
        wrap(method, *args)
      end
    end
  end

  def wrap(method, *args)
  end
end

class Other
  METHODS.each do |method|
    define_method(method) {}
  end
end

Wrapper.new.enable
Wrapper.new.disable
