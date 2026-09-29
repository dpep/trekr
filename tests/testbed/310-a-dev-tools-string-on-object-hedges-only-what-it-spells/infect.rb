class Module
  def infect(meth, new_name)
    extra = "ruby2_keywords %p" % [new_name]
    class_eval <<-EOM, __FILE__, __LINE__ + 1
      def #{new_name}(*args)
        send(:#{meth}, *args)
      end
      #{extra}
    EOM
  end
end
