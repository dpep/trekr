class Pretty
  def self.pretty!
    define_method :mu_pp, &:pretty_inspect
  end

  def self.make
    define_method(:made) { 1 }
  end
end

class Kid < Pretty
end

class Setup
  def self.setup(mod)
    mod.singleton_class.instance_eval do
      define_method(:namespace) { 1 }
    end
  end
end

module Single
  def self.go(name)
    define_singleton_method(name) { 1 }
  end
end

class Sanitized
  [:a, :b].each do |m|
    meth_name = "sanitized_#{m}"
    define_method(meth_name) { 1 }
    define_method("#{meth_name}=") { |_| 1 }
  end
end

class Renderers
  def self.add(key, &block)
    define_method(renderer_name(key), &block)
  end

  def self.renderer_name(key)
    "_render_with_#{key}"
  end
end

class Both
  def self.any(name)
    define_method(name) {}
  end

  def self.suffixed(name)
    define_method("#{name}_x") {}
  end
end

class Invokes
  def self.invoke_from(names)
    names.each do |name|
      class_eval <<-RUBY
        def _invoke_#{name.to_s.gsub(/\W/, "_")}
        end
      RUBY
    end
  end
end

class Templates
  def self.fresh
    Class.new(self) { define_method(:container) { 1 } }
  end
end
