class WidgetTest
  def test_rebound
    target = Target.new
    target.instance_eval do
      def rebound_helper
        :rebound
      end
    end
    target.rebound_helper
  end

  def test_hooked
    Hooks.on_load(:widget) do
      def hooked_helper
        :hooked
      end
    end
  end

  def test_built
    built = Class.new do
      def built_helper
        :built
      end
    end
    built.new.built_helper
  end

  def test_kept
    [1].each do
      def kept_helper
        :kept
      end
    end
  end
end
