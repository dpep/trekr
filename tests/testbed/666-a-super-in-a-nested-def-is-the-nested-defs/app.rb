class Widget
  def patch_arrays
    Array.class_eval do
      def respond_to_missing?(name, include_private = false)
        super
      end
    end
  end

  def run
    super
  end
end
