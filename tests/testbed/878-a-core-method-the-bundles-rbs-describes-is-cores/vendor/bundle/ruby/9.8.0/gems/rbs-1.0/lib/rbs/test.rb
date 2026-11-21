unless ::Module.private_instance_methods.include?(:ruby2_keywords)
  class Module
    private
    def ruby2_keywords(*)
    end
  end
end
