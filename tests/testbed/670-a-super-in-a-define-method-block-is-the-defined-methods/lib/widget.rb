class Widget
  def install
    self.class.send(:define_method, :zz) { super() }
  end

  def direct
    obj = Object.new
    obj.__send__(:define_singleton_method, :yy) do
      super()
    end
  end

  def run
    super
  end
end
