class Holed
  def self.proxy(names)
    names.each do |name|
      class_eval <<~RUBY
        def #{name}_proxy
          send(:#{name})
        end
      RUBY
    end
  end

  def compute = 1
end
