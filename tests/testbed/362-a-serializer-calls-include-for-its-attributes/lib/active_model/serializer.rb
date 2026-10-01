module ActiveModel
  class Serializer
    INCLUDE_METHODS = {}

    def self.attributes(*attrs)
      attrs.each { |attr| define_include_method(attr) }
    end

    def self.has_one(*attrs)
      attrs.each { |attr| define_include_method(attr) }
    end

    def self.define_include_method(name)
      method = "include_#{name}?".to_sym
      INCLUDE_METHODS[name] = method
      define_method(method) { true } unless method_defined?(method)
    end

    def include?(name)
      send INCLUDE_METHODS[name]
    end
  end
end
